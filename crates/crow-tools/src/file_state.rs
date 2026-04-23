//! File state tracking for staleness detection.
//!
//! Tracks when files are read by the agent. Before any edit or write,
//! we check if the file has been modified externally since the last read.
//! This prevents the agent from clobbering user changes.
//!
//! Inspired by Yomi's `FileStateStore` pattern.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Thread-safe file state tracker.
/// Records mtime when files are read, enabling staleness detection before edits.
pub struct FileStateStore {
    inner: Mutex<HashMap<PathBuf, u64>>,
}

impl Default for FileStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStateStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Record that a file was read at the given mtime (ms since epoch).
    pub fn record(&self, path: PathBuf, mtime_ms: u64) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(path, mtime_ms);
        }
    }

    /// Check if a file has been modified since the last recorded read.
    /// Returns true if the file is stale (mtime has changed).
    pub fn is_stale(&self, path: &Path, current_mtime_ms: u64) -> bool {
        if let Ok(map) = self.inner.lock() {
            if let Some(&recorded) = map.get(path) {
                return current_mtime_ms != recorded;
            }
        }
        false // If never recorded, we don't know — don't block
    }

    /// Check if a file path has ever been recorded (read).
    pub fn has_recorded(&self, path: &Path) -> bool {
        if let Ok(map) = self.inner.lock() {
            return map.contains_key(path);
        }
        false
    }
}

/// Get file modification time in milliseconds since epoch.
pub async fn get_file_mtime(path: &Path) -> u64 {
    tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_record_and_check() {
        let store = FileStateStore::new();
        let path = PathBuf::from("/tmp/test_file.txt");

        // Not recorded yet
        assert!(!store.has_recorded(&path));

        // Record
        store.record(path.clone(), 1000);
        assert!(store.has_recorded(&path));

        // Not stale when mtime matches
        assert!(!store.is_stale(&path, 1000));

        // Stale when mtime differs
        assert!(store.is_stale(&path, 2000));
    }

    #[tokio::test]
    async fn test_get_file_mtime() {
        let mut f = NamedTempFile::new().expect("create temp file");
        writeln!(f, "hello").expect("write");
        let mtime = get_file_mtime(f.path()).await;
        assert!(mtime > 0);
    }
}
