//! File safety utilities.
//!
//! Provides safety checks for file operations, ported from claw-code's
//! `file_ops.rs`. These utilities prevent the agent from:
//! - Reading binary files (which produce gibberish in the LLM context)
//! - Exceeding file size limits (which blow out context or memory)
//! - Escaping the workspace boundary via `../` traversal or symlinks

use std::io::{self, Read};
use std::path::Path;

/// Maximum file size that can be read (10 MB).
pub const MAX_READ_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum file size that can be written (10 MB).
pub const MAX_WRITE_SIZE: usize = 10 * 1024 * 1024;

/// Check whether a file appears to contain binary content by examining
/// the first 8KB chunk for NUL bytes.
///
/// This is a conservative heuristic: some text files with embedded NUL
/// bytes (e.g., SQLite databases posing as text) will be flagged as binary.
/// This is intentional — dumping binary content into an LLM context
/// wastes tokens and produces nonsensical output.
///
/// Ported from claw-code's `is_binary_file()`.
pub fn is_binary_file(path: &Path) -> io::Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let bytes_read = file.read(&mut buffer)?;
    Ok(buffer[..bytes_read].contains(&0))
}

/// Validate that a file does not exceed the maximum read size.
///
/// Returns `Ok(file_size)` if the file is within bounds, or an error
/// with a descriptive message if it's too large.
pub fn validate_file_size(path: &Path, max_bytes: u64) -> io::Result<u64> {
    let metadata = std::fs::metadata(path)?;
    let size = metadata.len();
    if size > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "File '{}' is too large ({} bytes, max {} bytes). \
                 Use a more targeted read (e.g., head/tail/grep) to extract the relevant sections.",
                path.display(),
                size,
                max_bytes
            ),
        ));
    }
    Ok(size)
}

/// Validate that a resolved path stays within the given workspace root.
///
/// Resolves symlinks and `../` traversal to detect escape attempts.
/// Returns the canonical path on success, or an error if the path
/// escapes the workspace boundary.
///
/// Ported from claw-code's `validate_workspace_boundary()`.
pub fn validate_workspace_boundary(
    path: &Path,
    workspace_root: &Path,
) -> io::Result<std::path::PathBuf> {
    // Resolve to absolute
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };

    // Canonicalize both for accurate comparison.
    // If the target doesn't exist yet (new file), check the parent.
    let canonical_ws = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

    let canonical_target = resolved
        .canonicalize()
        .or_else(|_| {
            // File doesn't exist yet — check the parent directory
            resolved
                .parent()
                .and_then(|p| p.canonicalize().ok())
                .map(|p| p.join(resolved.file_name().unwrap_or_default()))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot resolve parent"))
        })
        .unwrap_or(resolved);

    if !canonical_target.starts_with(&canonical_ws) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Path '{}' escapes workspace boundary '{}'",
                canonical_target.display(),
                canonical_ws.display()
            ),
        ));
    }

    Ok(canonical_target)
}

/// Quick check that a file is safe to read: exists, within bounds, not binary.
///
/// Returns `Ok(())` if all checks pass, or a human-readable error describing
/// the first failing check.
pub fn preflight_read(path: &Path, workspace_root: &Path) -> Result<(), String> {
    // 1. Workspace boundary
    validate_workspace_boundary(path, workspace_root)
        .map_err(|e| format!("Workspace boundary violation: {e}"))?;

    // 2. File exists
    if !path.exists() {
        return Err(format!("File does not exist: {}", path.display()));
    }

    // 3. Not a directory
    if path.is_dir() {
        return Err(format!(
            "Path is a directory, not a file: {}",
            path.display()
        ));
    }

    // 4. File size
    validate_file_size(path, MAX_READ_SIZE).map_err(|e| format!("File size check failed: {e}"))?;

    // 5. Binary detection
    match is_binary_file(path) {
        Ok(true) => Err(format!(
            "File '{}' appears to be binary. Use a hex viewer or specific tool instead.",
            path.display()
        )),
        Ok(false) => Ok(()),
        Err(e) => Err(format!("Cannot check file type: {e}")),
    }
}

/// Quick check that a file path is safe to write.
///
/// Validates workspace boundary and content size. Does NOT check binary
/// status (the agent may legitimately create binary files).
pub fn preflight_write(
    path: &Path,
    content_len: usize,
    workspace_root: &Path,
) -> Result<(), String> {
    // 1. Workspace boundary
    validate_workspace_boundary(path, workspace_root)
        .map_err(|e| format!("Workspace boundary violation: {e}"))?;

    // 2. Content size
    if content_len > MAX_WRITE_SIZE {
        return Err(format!(
            "Content too large to write ({content_len} bytes, max {MAX_WRITE_SIZE} bytes). \
             Break into smaller files or use incremental writes."
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_binary_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("binary.bin");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(&[0x00, 0x01, 0x02, 0x00, 0xFF]).expect("write");
        assert!(is_binary_file(&path).expect("check"));
    }

    #[test]
    fn detects_text_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("text.txt");
        std::fs::write(&path, "Hello, world!\nThis is text.\n").expect("write");
        assert!(!is_binary_file(&path).expect("check"));
    }

    #[test]
    fn validates_file_size_within_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "small content").expect("write");
        assert!(validate_file_size(&path, MAX_READ_SIZE).is_ok());
    }

    #[test]
    fn rejects_oversized_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "content").expect("write");
        // Set max to 1 byte to trigger rejection
        assert!(validate_file_size(&path, 1).is_err());
    }

    #[test]
    fn workspace_boundary_allows_inside() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("inside.txt");
        std::fs::write(&file, "ok").expect("write");
        assert!(validate_workspace_boundary(&file, dir.path()).is_ok());
    }

    #[test]
    fn workspace_boundary_blocks_outside() {
        let dir = tempfile::tempdir().expect("tempdir");
        // /etc/passwd is outside any tempdir
        let result = validate_workspace_boundary(Path::new("/etc/passwd"), dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn preflight_read_catches_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("binary.dat");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(&[0x00, 0x01, 0x02]).expect("write");
        let result = preflight_read(&path, dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("binary"));
    }

    #[test]
    fn preflight_read_passes_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("code.rs");
        std::fs::write(&path, "fn main() {}").expect("write");
        assert!(preflight_read(&path, dir.path()).is_ok());
    }

    #[test]
    fn preflight_write_rejects_oversized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.txt");
        let result = preflight_write(&path, MAX_WRITE_SIZE + 1, dir.path());
        assert!(result.is_err());
    }
}
