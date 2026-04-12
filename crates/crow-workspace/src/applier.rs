//! Physical sandbox mutation applier.
//!
//! This module is the **only** place that performs physical I/O against
//! a materialized sandbox. `crow-patch` (L0) defines pure intent types;
//! this module executes them against a `SandboxHandle` with full
//! precondition enforcement and Unlink-on-Write isolation discipline.

use crow_materialize::{MaterializationDriver, SandboxHandle};
use crow_patch::{
    ConflictStrategy, DiffHunk, EditOp, FilePrecondition, IntentPlan, PreconditionState,
};
use std::fs;
use std::io;
use std::path::Path;

// ─── Error ──────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("I/O error on {path}: {source}")]
    Io { path: String, source: io::Error },
    #[error("precondition failed: {0}")]
    PreconditionFailed(String),
    #[error("hunk apply failed at line {line} of {path}: {reason}")]
    HunkConflict {
        path: String,
        line: usize,
        reason: String,
    },
}

impl ApplyError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

// ─── SHA-256 Helper ─────────────────────────────────────────────────

/// Compute a hex-encoded SHA-256 digest of the given bytes.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    // Encode as lowercase hex (64 chars for SHA-256)
    hash.iter().map(|b| format!("{:02x}", b)).collect()
}

// ─── Precondition Verification ──────────────────────────────────────

fn verify_precondition_state(path: &Path, state: &PreconditionState) -> Result<String, ApplyError> {
    let content = fs::read_to_string(path).map_err(|e| ApplyError::io(path, e))?;

    let actual_hash = sha256_hex(content.as_bytes());
    if actual_hash != state.content_hash {
        return Err(ApplyError::PreconditionFailed(format!(
            "{}: content hash mismatch (expected {}, got {})",
            path.display(),
            state.content_hash,
            actual_hash
        )));
    }

    if let Some(expected) = state.expected_line_count {
        let actual = content.lines().count();
        if actual != expected {
            return Err(ApplyError::PreconditionFailed(format!(
                "{}: line count mismatch (expected {}, got {})",
                path.display(),
                expected,
                actual
            )));
        }
    }

    Ok(content)
}

fn verify_file_precondition(
    path: &Path,
    precondition: &FilePrecondition,
) -> Result<(), ApplyError> {
    match precondition {
        FilePrecondition::MustNotExist => {
            if path.exists() {
                return Err(ApplyError::PreconditionFailed(format!(
                    "{} already exists",
                    path.display()
                )));
            }
        }
        FilePrecondition::MustExist => {
            if !path.exists() {
                return Err(ApplyError::PreconditionFailed(format!(
                    "{} does not exist",
                    path.display()
                )));
            }
        }
        FilePrecondition::MustExistWithHash(expected_hash) => {
            if !path.exists() {
                return Err(ApplyError::PreconditionFailed(format!(
                    "{} does not exist",
                    path.display()
                )));
            }
            let content = fs::read(path).map_err(|e| ApplyError::io(path, e))?;
            let actual = sha256_hex(&content);
            if actual != *expected_hash {
                return Err(ApplyError::PreconditionFailed(format!(
                    "{}: hash mismatch (expected {}, got {})",
                    path.display(),
                    expected_hash,
                    actual
                )));
            }
        }
    }
    Ok(())
}

// ─── Hunk Application ──────────────────────────────────────────────

/// Apply a sequence of non-overlapping hunks to a file's lines.
/// Hunks must be sorted by `original_start` ascending. Each hunk
/// specifies a 1-based start line, lines to remove, and lines to insert.
///
/// The original file's trailing-newline state is preserved: if the
/// original ended with `\n`, the result will too; if it did not, the
/// result will not.
fn apply_hunks(original: &str, hunks: &[DiffHunk], file_path: &str) -> Result<String, ApplyError> {
    if hunks.is_empty() {
        return Ok(original.to_string());
    }

    let trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = original.lines().map(String::from).collect();
    let mut offset: isize = 0;

    for hunk in hunks {
        let start = (hunk.original_start as isize - 1 + offset) as usize;

        // Verify that the lines to remove actually match the file content
        for (i, expected_line) in hunk.remove_lines.iter().enumerate() {
            let actual_idx = start + i;
            if actual_idx >= lines.len() {
                return Err(ApplyError::HunkConflict {
                    path: file_path.into(),
                    line: hunk.original_start + i,
                    reason: format!(
                        "expected line '{}' but file only has {} lines",
                        expected_line,
                        lines.len()
                    ),
                });
            }
            if lines[actual_idx] != *expected_line {
                return Err(ApplyError::HunkConflict {
                    path: file_path.into(),
                    line: hunk.original_start + i,
                    reason: format!(
                        "expected '{}', found '{}'",
                        expected_line, lines[actual_idx]
                    ),
                });
            }
        }

        let remove_count = hunk.remove_lines.len();
        lines.drain(start..start + remove_count);

        for (i, new_line) in hunk.insert_lines.iter().enumerate() {
            lines.insert(start + i, new_line.clone());
        }

        offset += hunk.insert_lines.len() as isize - remove_count as isize;
    }

    let mut result = lines.join("\n");
    if trailing_newline {
        result.push('\n');
    }
    Ok(result)
}

// ─── Core Applier ───────────────────────────────────────────────────

/// Apply an `IntentPlan` to a materialized sandbox.
///
/// Every `EditOp` has its preconditions strictly verified before any
/// mutation occurs. For `HardlinkTree` sandboxes, existing files are
/// unlinked before writing to prevent inode bridge pollution.
pub fn apply_plan_to_sandbox(plan: &IntentPlan, sandbox: &SandboxHandle) -> Result<(), ApplyError> {
    let root = sandbox.path().to_path_buf();
    let is_hardlinked = sandbox.driver() == MaterializationDriver::HardlinkTree;

    for op in &plan.operations {
        match op {
            EditOp::Modify {
                path,
                preconditions,
                hunks,
            } => {
                let abs_path = path.to_absolute(&root);

                // Strict precondition enforcement
                let original_content = verify_precondition_state(&abs_path, preconditions)?;

                // Apply hunks to produce new content
                let new_content = apply_hunks(&original_content, hunks, path.as_str())?;

                // [CRITICAL]: Break inode bridge before writing
                unlink_if_hardlinked(is_hardlinked, &abs_path)?;

                fs::write(&abs_path, new_content).map_err(|e| ApplyError::io(&abs_path, e))?;
            }
            EditOp::Create {
                path,
                content,
                precondition,
            } => {
                let abs_path = path.to_absolute(&root);
                verify_file_precondition(&abs_path, precondition)?;

                if let Some(parent) = abs_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| ApplyError::io(parent, e))?;
                }
                // [CRITICAL]: Even Create can overwrite if precondition
                // is not MustNotExist. Break inode bridge before writing.
                unlink_if_hardlinked(is_hardlinked, &abs_path)?;
                fs::write(&abs_path, content).map_err(|e| ApplyError::io(&abs_path, e))?;
            }
            EditOp::Rename {
                from,
                to,
                on_conflict,
                source_precondition,
                dest_precondition,
            } => {
                let abs_from = from.to_absolute(&root);
                let abs_to = to.to_absolute(&root);

                verify_file_precondition(&abs_from, source_precondition)?;
                verify_file_precondition(&abs_to, dest_precondition)?;

                // Check conflict strategy if destination exists
                if abs_to.exists() {
                    match on_conflict {
                        ConflictStrategy::Fail => {
                            return Err(ApplyError::PreconditionFailed(format!(
                                "rename target {} already exists (conflict strategy: Fail)",
                                abs_to.display()
                            )));
                        }
                        ConflictStrategy::Overwrite => {
                            unlink_if_hardlinked(is_hardlinked, &abs_to)?;
                            fs::remove_file(&abs_to).map_err(|e| ApplyError::io(&abs_to, e))?;
                        }
                    }
                }

                if let Some(parent) = abs_to.parent() {
                    fs::create_dir_all(parent).map_err(|e| ApplyError::io(parent, e))?;
                }

                fs::rename(&abs_from, &abs_to).map_err(|e| ApplyError::io(&abs_from, e))?;
            }
            EditOp::Delete { path, precondition } => {
                let abs_path = path.to_absolute(&root);
                verify_file_precondition(&abs_path, precondition)?;

                if abs_path.is_dir() {
                    fs::remove_dir_all(&abs_path).map_err(|e| ApplyError::io(&abs_path, e))?;
                } else if abs_path.exists() {
                    fs::remove_file(&abs_path).map_err(|e| ApplyError::io(&abs_path, e))?;
                }
            }
        }
    }
    Ok(())
}

/// If the sandbox is hardlinked, unlink the file before writing to
/// sever the physical inode bridge to the source repository.
fn unlink_if_hardlinked(is_hardlinked: bool, path: &Path) -> Result<(), ApplyError> {
    if is_hardlinked && path.exists() {
        fs::remove_file(path).map_err(|e| ApplyError::io(path, e))?;
    }
    Ok(())
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crow_patch::{DiffHunk, FilePrecondition};
    use std::fs;
    use tempfile::TempDir;

    /// Helper: write a file inside a dir and return the path
    fn write_file(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }

    #[test]
    fn apply_hunks_pure_insertion() {
        let original = "line 1\nline 2\nline 3\n";
        let hunks = vec![DiffHunk {
            original_start: 2,
            remove_lines: vec![],
            insert_lines: vec!["inserted".into()],
        }];
        let result = apply_hunks(original, &hunks, "test.rs").unwrap();
        assert!(result.contains("inserted"));
        assert!(result.contains("line 1"));
        assert!(result.contains("line 2"));
        assert!(result.contains("line 3"));
    }

    #[test]
    fn apply_hunks_pure_deletion() {
        let original = "line 1\nline 2\nline 3\n";
        let hunks = vec![DiffHunk {
            original_start: 2,
            remove_lines: vec!["line 2".into()],
            insert_lines: vec![],
        }];
        let result = apply_hunks(original, &hunks, "test.rs").unwrap();
        assert!(!result.contains("line 2"));
        assert!(result.contains("line 1"));
        assert!(result.contains("line 3"));
    }

    #[test]
    fn apply_hunks_replacement() {
        let original = "fn old() {}\nfn keep() {}\n";
        let hunks = vec![DiffHunk {
            original_start: 1,
            remove_lines: vec!["fn old() {}".into()],
            insert_lines: vec!["fn new() {}".into()],
        }];
        let result = apply_hunks(original, &hunks, "test.rs").unwrap();
        assert!(result.contains("fn new() {}"));
        assert!(!result.contains("fn old() {}"));
        assert!(result.contains("fn keep() {}"));
    }

    #[test]
    fn apply_hunks_context_mismatch_is_error() {
        let original = "line 1\nline 2\n";
        let hunks = vec![DiffHunk {
            original_start: 1,
            remove_lines: vec!["WRONG CONTEXT".into()],
            insert_lines: vec!["new".into()],
        }];
        let result = apply_hunks(original, &hunks, "test.rs");
        assert!(result.is_err());
        match result.unwrap_err() {
            ApplyError::HunkConflict { path, .. } => assert_eq!(path, "test.rs"),
            other => panic!("expected HunkConflict, got {:?}", other),
        }
    }

    #[test]
    fn create_enforces_must_not_exist() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "existing.rs", "content");

        let abs = dir.path().join("existing.rs");
        let err = verify_file_precondition(&abs, &FilePrecondition::MustNotExist);
        assert!(err.is_err());
    }

    #[test]
    fn delete_enforces_must_exist() {
        let dir = TempDir::new().unwrap();
        let abs = dir.path().join("ghost.rs");
        let err = verify_file_precondition(&abs, &FilePrecondition::MustExist);
        assert!(err.is_err());
    }
}
