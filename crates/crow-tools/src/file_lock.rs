//! Cross-process file locking for edit/write tool safety.
//!
//! Ported from yomi's `tools/file_lock.rs`.
//! Prevents concurrent file modifications by multiple agent processes
//! sharing the same workspace. Uses Rust 1.89+ `std::fs::File::lock()`.
//!
//! ## Usage
//!
//! ```no_run
//! # use std::path::Path;
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let _guard = crow_tools::file_lock::lock_exclusive(Path::new("foo.rs")).await?;
//! // ... write to file ...
//! // lock released automatically when _guard is dropped
//! # Ok(())
//! # }
//! ```

use std::fs::File;
use std::path::Path;

/// Default timeout for file lock acquisition.
pub const DEFAULT_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Errors that can occur during file lock operations.
#[derive(Debug)]
pub enum FileLockError {
    /// Failed to open the file for locking.
    OpenError(std::io::Error),
    /// Failed to acquire the lock (e.g., OS error).
    LockError(std::io::Error),
    /// Lock acquisition timed out.
    Timeout,
}

impl std::fmt::Display for FileLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenError(e) => write!(f, "Failed to open file for locking: {e}"),
            Self::LockError(e) => write!(f, "Failed to acquire file lock: {e}"),
            Self::Timeout => write!(
                f,
                "Timeout waiting for file lock (another process may be holding it)"
            ),
        }
    }
}

impl std::error::Error for FileLockError {}

/// RAII guard that releases the file lock when dropped.
pub struct FileLockGuard {
    _file: File,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // Lock is automatically released when the file handle is closed.
        // We call unlock() explicitly for clarity.
        let _ = self._file.unlock();
    }
}

/// Acquire an exclusive (write) lock on a file.
///
/// Blocks until the lock is acquired or an error occurs.
/// The lock is automatically released when the returned guard is dropped.
pub async fn lock_exclusive(path: &Path) -> Result<FileLockGuard, FileLockError> {
    let path = path.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let file = File::options()
            .read(true)
            .write(true)
            .create(false)
            .open(&path)
            .map_err(FileLockError::OpenError)?;

        file.lock().map_err(FileLockError::LockError)?;

        Ok(FileLockGuard { _file: file })
    })
    .await
    .map_err(|e| FileLockError::LockError(std::io::Error::other(format!("Task join error: {e}"))))?
}

/// Acquire a shared (read) lock on a file.
///
/// Multiple readers can hold shared locks simultaneously, but
/// a shared lock blocks exclusive lock acquisition.
pub async fn lock_shared(path: &Path) -> Result<FileLockGuard, FileLockError> {
    let path = path.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let file = File::options()
            .read(true)
            .write(false)
            .open(&path)
            .map_err(FileLockError::OpenError)?;

        file.lock_shared().map_err(FileLockError::LockError)?;

        Ok(FileLockGuard { _file: file })
    })
    .await
    .map_err(|e| FileLockError::LockError(std::io::Error::other(format!("Task join error: {e}"))))?
}

/// Acquire an exclusive lock with a timeout.
///
/// Returns `FileLockError::Timeout` if the lock cannot be acquired
/// within the specified duration.
pub async fn lock_exclusive_timeout(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<FileLockGuard, FileLockError> {
    tokio::time::timeout(timeout, lock_exclusive(path))
        .await
        .map_err(|_| FileLockError::Timeout)?
}

/// Acquire a shared lock with a timeout.
pub async fn lock_shared_timeout(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<FileLockGuard, FileLockError> {
    tokio::time::timeout(timeout, lock_shared(path))
        .await
        .map_err(|_| FileLockError::Timeout)?
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn exclusive_lock_acquires() {
        let mut temp = NamedTempFile::new().expect("create temp file");
        writeln!(temp, "test content").expect("write");
        let _guard = lock_exclusive(temp.path()).await.expect("lock");
    }

    #[tokio::test]
    async fn shared_lock_acquires() {
        let mut temp = NamedTempFile::new().expect("create temp file");
        writeln!(temp, "test content").expect("write");
        let _guard = lock_shared(temp.path()).await.expect("lock");
    }

    #[tokio::test]
    async fn guard_releases_on_drop() {
        let mut temp = NamedTempFile::new().expect("create temp file");
        writeln!(temp, "test content").expect("write");
        let path = temp.path().to_path_buf();

        {
            let _guard = lock_exclusive(&path).await.expect("first lock");
        }
        // Should be able to re-acquire after drop
        let _guard2 = lock_exclusive(&path).await.expect("second lock");
    }

    #[tokio::test]
    async fn nonexistent_file_errors() {
        let result = lock_exclusive(Path::new("/nonexistent/file.txt")).await;
        assert!(result.is_err());
    }
}
