//! File state tracking for staleness detection.
//!
//! Tracks when files are read by the agent. Before any edit or write,
//! we check if the file has been modified externally since the last read.
//! This prevents the agent from clobbering user changes.
//!
//! Inspired by Yomi's `FileStateStore` pattern, with content-hash augmentation
//! to catch sub-second edits that mtime alone might miss (filesystem
//! resolution can be 1s on HFS+, FAT, NFS).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A file's recorded state at the time of last read.
#[derive(Debug, Clone)]
struct ReadState {
    /// Mtime in milliseconds since epoch.
    mtime_ms: u64,
    /// Stable content fingerprint (FxHash of bytes). 0 means "not computed".
    content_hash: u64,
}

/// Thread-safe file state tracker.
/// Records mtime + content hash when files are read, enabling robust
/// staleness detection before edits.
pub struct FileStateStore {
    inner: Mutex<HashMap<PathBuf, ReadState>>,
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

    /// Record that a file was read at the given mtime (legacy entry — no
    /// content hash). Prefer `record_with_hash` to enable hash-based
    /// staleness detection.
    pub fn record(&self, path: PathBuf, mtime_ms: u64) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(
                path,
                ReadState {
                    mtime_ms,
                    content_hash: 0,
                },
            );
        }
    }

    /// Record a read with both mtime and content hash. The content hash
    /// is the authoritative staleness check — mtime is only a fast hint.
    pub fn record_with_hash(&self, path: PathBuf, mtime_ms: u64, content_hash: u64) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(
                path,
                ReadState {
                    mtime_ms,
                    content_hash,
                },
            );
        }
    }

    /// Check if a file has been modified since the last recorded read,
    /// using mtime alone (fast path; may miss sub-second edits).
    pub fn is_stale(&self, path: &Path, current_mtime_ms: u64) -> bool {
        if let Ok(map) = self.inner.lock() {
            if let Some(state) = map.get(path) {
                return current_mtime_ms != state.mtime_ms;
            }
        }
        false
    }

    /// Authoritative staleness check using both mtime and content hash.
    /// Returns true if the file has changed since the last recorded read.
    /// Falls back to mtime-only if no hash was recorded for this path.
    pub fn is_stale_with_hash(
        &self,
        path: &Path,
        current_mtime_ms: u64,
        current_hash: u64,
    ) -> bool {
        if let Ok(map) = self.inner.lock() {
            if let Some(state) = map.get(path) {
                if state.content_hash != 0 {
                    // Hash was recorded — trust it as the source of truth.
                    return state.content_hash != current_hash;
                }
                return current_mtime_ms != state.mtime_ms;
            }
        }
        false
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

/// Compute a fast non-cryptographic 64-bit content hash for staleness
/// detection. Uses Rust's default Hasher (SipHash-13) which is good
/// enough for change detection — we are not protecting against
/// adversaries, only against silent file mutations.
pub fn hash_content(content: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Compute the content hash of a file on disk asynchronously.
/// Returns 0 on read failure (treated as "no hash available").
pub async fn hash_file_on_disk(path: &Path) -> u64 {
    match tokio::fs::read(path).await {
        Ok(bytes) => hash_content(&bytes),
        Err(_) => 0,
    }
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

        assert!(!store.has_recorded(&path));
        store.record(path.clone(), 1000);
        assert!(store.has_recorded(&path));
        assert!(!store.is_stale(&path, 1000));
        assert!(store.is_stale(&path, 2000));
    }

    #[test]
    fn test_hash_based_staleness() {
        let store = FileStateStore::new();
        let path = PathBuf::from("/tmp/hash_test.txt");

        let hash_a = hash_content(b"hello world");
        let hash_b = hash_content(b"hello world!");
        assert_ne!(hash_a, hash_b);

        store.record_with_hash(path.clone(), 1000, hash_a);
        // Same hash + same mtime → fresh
        assert!(!store.is_stale_with_hash(&path, 1000, hash_a));
        // Different hash even with same mtime → stale
        assert!(store.is_stale_with_hash(&path, 1000, hash_b));
        // Same hash even with different mtime → fresh (file touched but
        // unchanged; hash is authoritative)
        assert!(!store.is_stale_with_hash(&path, 9999, hash_a));
    }

    #[test]
    fn test_hash_fallback_to_mtime_when_no_hash() {
        let store = FileStateStore::new();
        let path = PathBuf::from("/tmp/no_hash_test.txt");
        // Record without hash
        store.record(path.clone(), 1000);
        // Should fall back to mtime check
        assert!(!store.is_stale_with_hash(&path, 1000, 0));
        assert!(store.is_stale_with_hash(&path, 2000, 0));
    }

    #[tokio::test]
    async fn test_get_file_mtime() {
        let mut f = NamedTempFile::new().expect("create temp file");
        writeln!(f, "hello").expect("write");
        let mtime = get_file_mtime(f.path()).await;
        assert!(mtime > 0);
    }

    #[tokio::test]
    async fn test_hash_file_on_disk() {
        let mut f = NamedTempFile::new().expect("create temp file");
        writeln!(f, "hello world").expect("write");
        let h1 = hash_file_on_disk(f.path()).await;
        let h2 = hash_file_on_disk(f.path()).await;
        assert_eq!(h1, h2);
        assert_ne!(h1, 0);
    }
}
