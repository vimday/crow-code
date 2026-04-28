//! Observable agent status (Codex pattern).
//!
//! Provides a tokio `watch`-based observable status enum that allows
//! any number of subscribers to be notified when the agent's lifecycle
//! state changes. This replaces ad-hoc boolean flags like `is_streaming`
//! with a proper state machine.
//!
//! # Usage
//!
//! ```ignore
//! let status = AgentStatusTracker::new();
//! let rx = status.subscribe();
//!
//! status.set(AgentStatus::Running).await;
//! assert_eq!(*rx.borrow(), AgentStatus::Running);
//!
//! status.set(AgentStatus::Completed(None)).await;
//! assert!(status.is_final().await);
//! ```

use tokio::sync::watch;

/// Agent lifecycle status.
///
/// Mirrors Codex's `AgentStatus` enum with the states relevant
/// to crow-code's session lifecycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent is initialized but has not started processing.
    #[default]
    PendingInit,
    /// Agent is actively processing a turn.
    Running,
    /// Agent completed successfully with an optional summary.
    Completed(Option<String>),
    /// Agent was interrupted by the user (ESC / Ctrl+C).
    Interrupted,
    /// Agent encountered a fatal error.
    Errored(String),
    /// Agent is shutting down.
    Shutdown,
}

impl AgentStatus {
    /// Returns true if this is a terminal state.
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::PendingInit | Self::Running)
    }

    /// Returns true if the agent is actively processing.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PendingInit => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Completed(msg) => {
                if let Some(msg) = msg {
                    write!(f, "completed: {msg}")
                } else {
                    write!(f, "completed")
                }
            }
            Self::Interrupted => write!(f, "interrupted"),
            Self::Errored(e) => write!(f, "error: {e}"),
            Self::Shutdown => write!(f, "shutdown"),
        }
    }
}

/// Observable agent status tracker using `tokio::sync::watch`.
///
/// Allows multiple subscribers to observe state transitions without
/// polling. Each call to `set()` notifies all watchers.
pub struct AgentStatusTracker {
    tx: watch::Sender<AgentStatus>,
    rx: watch::Receiver<AgentStatus>,
}

impl Default for AgentStatusTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentStatusTracker {
    /// Create a new tracker in `PendingInit` state.
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(AgentStatus::PendingInit);
        Self { tx, rx }
    }

    /// Update the agent status. All subscribers are notified.
    pub fn set(&self, status: AgentStatus) {
        // watch::Sender::send only fails if all receivers are dropped,
        // which is fine — we just silently ignore.
        let _ = self.tx.send(status);
    }

    /// Get the current status.
    pub fn current(&self) -> AgentStatus {
        self.rx.borrow().clone()
    }

    /// Returns true if the agent is in a terminal state.
    pub fn is_final(&self) -> bool {
        self.rx.borrow().is_final()
    }

    /// Subscribe to status changes. Returns a `watch::Receiver` that
    /// can be used in `tokio::select!` via `.changed().await`.
    pub fn subscribe(&self) -> watch::Receiver<AgentStatus> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_lifecycle() {
        let tracker = AgentStatusTracker::new();
        assert_eq!(tracker.current(), AgentStatus::PendingInit);
        assert!(!tracker.is_final());

        tracker.set(AgentStatus::Running);
        assert_eq!(tracker.current(), AgentStatus::Running);
        assert!(!tracker.is_final());

        tracker.set(AgentStatus::Completed(Some("done".into())));
        assert!(tracker.is_final());
    }

    #[test]
    fn test_subscribe() {
        let tracker = AgentStatusTracker::new();
        let rx = tracker.subscribe();
        assert_eq!(*rx.borrow(), AgentStatus::PendingInit);

        tracker.set(AgentStatus::Running);
        assert_eq!(*rx.borrow(), AgentStatus::Running);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", AgentStatus::PendingInit), "pending");
        assert_eq!(format!("{}", AgentStatus::Running), "running");
        assert_eq!(
            format!("{}", AgentStatus::Errored("timeout".into())),
            "error: timeout"
        );
    }

    #[test]
    fn test_is_final() {
        assert!(!AgentStatus::PendingInit.is_final());
        assert!(!AgentStatus::Running.is_final());
        assert!(AgentStatus::Completed(None).is_final());
        assert!(AgentStatus::Interrupted.is_final());
        assert!(AgentStatus::Errored("x".into()).is_final());
        assert!(AgentStatus::Shutdown.is_final());
    }
}
