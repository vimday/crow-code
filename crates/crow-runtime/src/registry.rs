//! Task and Team Registries for Subagent Orchestration.
//!
//! Replaces simple `tokio::spawn` fire-and-forget with managed lifecycle states.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Suspended,
    Completed,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct AgentTask {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: TaskStatus,
    pub output: Option<String>,
}

impl AgentTask {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            description: description.into(),
            status: TaskStatus::Pending,
            output: None,
        }
    }
}

/// Thread-safe registry for managing autonomous tasks.
#[derive(Default, Clone)]
pub struct TaskRegistry {
    tasks: Arc<RwLock<HashMap<String, AgentTask>>>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, task: AgentTask) -> String {
        let id = task.id.clone();
        self.tasks.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id.clone(), task);
        id
    }

    pub fn get(&self, id: &str) -> Option<AgentTask> {
        self.tasks.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(id).cloned()
    }

    pub fn update_status(&self, id: &str, status: TaskStatus) {
        if let Some(task) = self.tasks.write().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(id) {
            task.status = status;
        }
    }

    pub fn list(&self) -> Vec<AgentTask> {
        self.tasks.read().unwrap_or_else(std::sync::PoisonError::into_inner).values().cloned().collect()
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
        self.teams.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id.clone(), team);
        id
    }
}
