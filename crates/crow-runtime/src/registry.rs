//! Task and Team Registries for Subagent Orchestration.
//!
//! Replaces simple `tokio::spawn` fire-and-forget with managed lifecycle states.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Suspended,
    Completed,
    Failed(String),
}

impl TaskStatus {
    fn is_active(&self) -> bool {
        matches!(self, Self::Pending | Self::Running | Self::Suspended)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTaskKind {
    Explore,
    Plan,
    Execute,
    Review,
    Verify,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskPreview {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: AgentTaskKind,
    pub status: TaskStatus,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    ActiveLimitReached { limit: usize },
    MissingTask { id: String },
}

#[derive(Debug, Clone)]
pub struct AgentTask {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: TaskStatus,
    pub output: Option<String>,
    pub kind: AgentTaskKind,
    pub preview: Option<String>,
    pub started_at_ms: Option<u128>,
    pub completed_at_ms: Option<u128>,
}

impl AgentTask {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self::with_kind(name, description, AgentTaskKind::Execute)
    }

    pub fn with_kind(
        name: impl Into<String>,
        description: impl Into<String>,
        kind: AgentTaskKind,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            description: description.into(),
            status: TaskStatus::Pending,
            output: None,
            kind,
            preview: None,
            started_at_ms: None,
            completed_at_ms: None,
        }
    }
}

/// Thread-safe registry for managing autonomous tasks.
#[derive(Clone)]
pub struct TaskRegistry {
    tasks: Arc<RwLock<HashMap<String, AgentTask>>>,
    max_active: usize,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::with_limit(64)
    }
}

pub struct TaskReservation {
    id: String,
    registry: TaskRegistry,
    committed: bool,
}

impl TaskReservation {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn commit(mut self) -> String {
        self.committed = true;
        self.id.clone()
    }
}

impl Drop for TaskReservation {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.registry.remove(&self.id);
        }
    }
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limit(max_active: usize) -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            max_active: max_active.max(1),
        }
    }

    pub fn reserve(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        kind: AgentTaskKind,
    ) -> Result<TaskReservation, RegistryError> {
        let mut tasks = self
            .tasks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active = tasks
            .values()
            .filter(|task| task.status.is_active())
            .count();
        if active >= self.max_active {
            return Err(RegistryError::ActiveLimitReached {
                limit: self.max_active,
            });
        }
        let task = AgentTask::with_kind(name, description, kind);
        let id = task.id.clone();
        tasks.insert(id.clone(), task);
        Ok(TaskReservation {
            id,
            registry: self.clone(),
            committed: false,
        })
    }

    pub fn register(&self, task: AgentTask) -> String {
        let id = task.id.clone();
        self.tasks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone(), task);
        id
    }

    pub fn get(&self, id: &str) -> Option<AgentTask> {
        self.tasks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    pub fn update_status(&self, id: &str, status: TaskStatus) {
        let _ = self.set_status(id, status);
    }

    pub fn set_running(&self, id: &str) -> Result<(), RegistryError> {
        self.set_status_with_time(id, TaskStatus::Running, true, false)
    }

    pub fn set_completed(&self, id: &str, output: Option<String>) -> Result<(), RegistryError> {
        let mut tasks = self
            .tasks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let task = tasks
            .get_mut(id)
            .ok_or_else(|| RegistryError::MissingTask { id: id.to_string() })?;
        task.status = TaskStatus::Completed;
        task.output = output;
        task.completed_at_ms = Some(now_ms());
        Ok(())
    }

    pub fn set_failed(&self, id: &str, error: String) -> Result<(), RegistryError> {
        self.set_status_with_time(id, TaskStatus::Failed(error), false, true)
    }

    pub fn set_preview(&self, id: &str, preview: impl Into<String>) -> Result<(), RegistryError> {
        let mut tasks = self
            .tasks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let task = tasks
            .get_mut(id)
            .ok_or_else(|| RegistryError::MissingTask { id: id.to_string() })?;
        task.preview = Some(preview.into());
        Ok(())
    }

    pub fn active_previews(&self, limit: usize) -> Vec<AgentTaskPreview> {
        let mut previews: Vec<_> = self
            .tasks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|task| task.status.is_active())
            .map(|task| AgentTaskPreview {
                id: task.id.clone(),
                name: task.name.clone(),
                description: task.description.clone(),
                kind: task.kind,
                status: task.status.clone(),
                preview: task.preview.clone(),
            })
            .collect();
        previews.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        previews.truncate(limit);
        previews
    }

    pub fn list(&self) -> Vec<AgentTask> {
        self.tasks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    fn set_status(&self, id: &str, status: TaskStatus) -> Result<(), RegistryError> {
        self.set_status_with_time(id, status, false, false)
    }

    fn set_status_with_time(
        &self,
        id: &str,
        status: TaskStatus,
        mark_started: bool,
        mark_completed: bool,
    ) -> Result<(), RegistryError> {
        let mut tasks = self
            .tasks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let task = tasks
            .get_mut(id)
            .ok_or_else(|| RegistryError::MissingTask { id: id.to_string() })?;
        task.status = status;
        if mark_started {
            task.started_at_ms = Some(now_ms());
        }
        if mark_completed {
            task.completed_at_ms = Some(now_ms());
        }
        Ok(())
    }

    fn remove(&self, id: &str) -> Result<(), RegistryError> {
        let removed = self
            .tasks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        if removed.is_some() {
            Ok(())
        } else {
            Err(RegistryError::MissingTask { id: id.to_string() })
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentTeam {
    pub id: String,
    pub name: String,
    pub members: Vec<String>, // task IDs or subagent IDs
}

/// Thread-safe registry for managing agent teams.
#[derive(Default, Clone)]
pub struct TeamRegistry {
    teams: Arc<RwLock<HashMap<String, AgentTeam>>>,
}

impl TeamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_team(&self, name: impl Into<String>) -> String {
        let team = AgentTeam {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            members: Vec::new(),
        };
        let id = team.id.clone();
        self.teams
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone(), team);
        id
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_releases_slot_on_drop() {
        let registry = TaskRegistry::with_limit(1);
        let first = registry.reserve("explore", "scan", AgentTaskKind::Explore);
        assert!(first.is_ok());
        let first = first.unwrap_or_else(|err| panic!("first slot should reserve: {err:?}"));
        assert!(matches!(
            registry.reserve("plan", "make plan", AgentTaskKind::Plan),
            Err(RegistryError::ActiveLimitReached { limit: 1 })
        ));
        drop(first);
        assert!(registry
            .reserve("plan", "make plan", AgentTaskKind::Plan)
            .is_ok());
    }

    #[test]
    fn committed_reservation_keeps_task() {
        let registry = TaskRegistry::with_limit(1);
        let reservation = registry.reserve("execute", "apply changes", AgentTaskKind::Execute);
        assert!(reservation.is_ok());
        let reservation =
            reservation.unwrap_or_else(|err| panic!("execute slot should reserve: {err:?}"));
        let id = reservation.id().to_string();
        let committed = reservation.commit();
        assert_eq!(committed, id);
        assert!(registry.get(&id).is_some());
    }

    #[test]
    fn active_previews_are_bounded_and_sorted() {
        let registry = TaskRegistry::with_limit(4);
        let reservation = registry.reserve("explore", "scan repository", AgentTaskKind::Explore);
        assert!(reservation.is_ok());
        let reservation =
            reservation.unwrap_or_else(|err| panic!("explore slot should reserve: {err:?}"));
        let id = reservation.id().to_string();
        let running_result = registry.set_running(&id);
        assert!(running_result.is_ok());
        let preview_result =
            registry.set_preview(&id, "Read crates/crow-runtime/src/turn_context.rs");
        assert!(preview_result.is_ok());
        let previews = registry.active_previews(8);
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].id, id);
        assert_eq!(previews[0].kind, AgentTaskKind::Explore);
    }
}
