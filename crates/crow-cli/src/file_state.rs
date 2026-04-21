#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// File state tracking constraint enforcing explicit model reconciliation.
/// Agents using ReadTool/GrepTool passively populate these MTime checkpoints.
/// Subsequent patch intents matching these paths must align chronologically.
#[derive(Debug, Clone, Default)]
pub struct FileStateStore {
    mtimes: Arc<RwLock<HashMap<String, u64>>>,
}

impl FileStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, path: impl Into<String>, mtime: u64) {
        self.mtimes.write().expect("Mtime lock poisoned").insert(path.into(), mtime);
    }

    pub fn get_mtime(&self, path: &str) -> Option<u64> {
        self.mtimes.read().expect("Mtime lock poisoned").get(path).copied()
    }

    pub fn has_recorded(&self, path: &str) -> bool {
        self.mtimes.read().expect("Mtime lock poisoned").contains_key(path)
    }

    pub fn is_stale(&self, path: &str, current_mtime: u64) -> bool {
        self.get_mtime(path) != Some(current_mtime)
    }

    pub fn clear(&self) {
        self.mtimes.write().expect("Mtime lock poisoned").clear();
    }
}
