use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use crow_brain::ChatMessage;
use crow_patch::WorkspacePath;

use super::artifact::AgentArtifactBundle;
use super::graph::AutoNodeId;
use super::AutoPhaseKind;
use crate::event::AgentEvent;

#[derive(Debug, Clone)]
pub struct AutoNodeExecutionRequest {
    pub run_id: String,
    pub node_id: AutoNodeId,
    pub agent_name: String,
    pub role: String,
    pub phase: AutoPhaseKind,
    pub task: String,
    pub focus_paths: Vec<WorkspacePath>,
    pub handoff_context: String,
    pub system_messages: Vec<ChatMessage>,
}

pub type AutoNodeEventSink = tokio::sync::mpsc::UnboundedSender<AgentEvent>;

pub trait AutoNodeExecutor: Send + Sync {
    fn execute_node(
        &self,
        request: AutoNodeExecutionRequest,
        event_sink: AutoNodeEventSink,
    ) -> Pin<Box<dyn Future<Output = Result<AgentArtifactBundle>> + Send + 'static>>;
}

#[derive(Debug, Default)]
pub struct AutoExecutionLimiter {
    active: AtomicUsize,
    max_parallel: usize,
}

impl AutoExecutionLimiter {
    pub fn new(max_parallel: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_parallel: max_parallel.max(1),
        }
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    pub fn max_parallel(&self) -> usize {
        self.max_parallel
    }

    pub fn try_acquire(self: &Arc<Self>) -> Result<AutoExecutionGuard> {
        let mut current = self.active.load(Ordering::Acquire);
        loop {
            if current >= self.max_parallel {
                bail!(
                    "auto execution capacity exhausted: {current}/{} active",
                    self.max_parallel
                );
            }
            match self.active.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(AutoExecutionGuard {
                        limiter: Arc::clone(self),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }
}

pub struct AutoExecutionGuard {
    limiter: Arc<AutoExecutionLimiter>,
}

impl Drop for AutoExecutionGuard {
    fn drop(&mut self) {
        self.limiter.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_releases_capacity_when_guard_drops() {
        let limiter = Arc::new(AutoExecutionLimiter::new(1));
        {
            let guard = limiter.try_acquire();
            assert!(guard.is_ok());
            assert_eq!(limiter.active(), 1);
            assert!(limiter.try_acquire().is_err());
        }

        assert_eq!(limiter.active(), 0);
        assert!(limiter.try_acquire().is_ok());
    }

    #[test]
    fn limiter_clamps_zero_parallelism_to_one() {
        let limiter = AutoExecutionLimiter::new(0);

        assert_eq!(limiter.max_parallel(), 1);
    }
}
