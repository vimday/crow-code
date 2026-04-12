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
/// Preserves the original file's line-ending style (LF vs CRLF) and
/// trailing-newline state. Uses elastic matching (trim_end comparison)
/// so trailing whitespace differences from LLM output do not falsely
/// reject otherwise-valid patches.
fn apply_hunks(original: &str, hunks: &[DiffHunk], file_path: &str) -> Result<String, ApplyError> {
    if hunks.is_empty() {
        return Ok(original.to_string());
    }

    // ── Structural validation ──────────────────────────────────
    for (i, hunk) in hunks.iter().enumerate() {
        if hunk.original_start == 0 {
            return Err(ApplyError::HunkConflict {
                path: file_path.into(),
                line: 0,
                reason: "original_start must be 1-based, got 0".into(),
            });
        }
        if i > 0 {
            let prev = &hunks[i - 1];
            let prev_end = prev.original_start + prev.remove_lines.len();
            if hunk.original_start < prev_end {
                return Err(ApplyError::HunkConflict {
                    path: file_path.into(),
                    line: hunk.original_start,
                    reason: format!(
                        "hunks must be sorted ascending and non-overlapping; \
                         hunk at line {} overlaps previous hunk ending at line {}",
                        hunk.original_start, prev_end
                    ),
                });
            }
        }
    }

    // ── Detect line ending style and trailing newline ───────────
    let line_ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = original.lines().map(String::from).collect();
    let mut offset: isize = 0;

    for hunk in hunks {
        let adjusted = hunk.original_start as isize - 1 + offset;
        if adjusted < 0 || adjusted as usize > lines.len() {
            return Err(ApplyError::HunkConflict {
                path: file_path.into(),
                line: hunk.original_start,
                reason: format!(
                    "adjusted start {} is out of bounds (file has {} lines after prior edits)",
                    adjusted,
                    lines.len()
                ),
            });
        }
        let start = adjusted as usize;

        // Elastic matching: trim trailing whitespace so LLM typos
        // (extra spaces) don't falsely reject valid patches.
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
            if lines[actual_idx].trim_end() != expected_line.trim_end() {
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

        // O(N) batch replacement via splice
        let remove_count = hunk.remove_lines.len();
        lines.splice(
            start..start + remove_count,
            hunk.insert_lines.iter().cloned(),
        );

        offset += hunk.insert_lines.len() as isize - remove_count as isize;
    }

    // Reconstruct with the original line-ending style
    let mut result = lines.join(line_ending);
    if trailing_newline {
        result.push_str(line_ending);
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
    use crow_patch::{DiffHunk, EditOp, FilePrecondition, IntentPlan, WorkspacePath};
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

    #[test]
    fn apply_hunks_rejects_zero_based_start() {
        let original = "line 1\nline 2\n";
        let hunks = vec![DiffHunk {
            original_start: 0,
            remove_lines: vec![],
            insert_lines: vec!["bad".into()],
        }];
        let result = apply_hunks(original, &hunks, "test.rs");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("1-based"), "got: {}", msg);
    }

    #[test]
    fn apply_hunks_rejects_overlapping() {
        let original = "a\nb\nc\nd\n";
        let hunks = vec![
            DiffHunk {
                original_start: 1,
                remove_lines: vec!["a".into(), "b".into()],
                insert_lines: vec!["x".into()],
            },
            DiffHunk {
                original_start: 2, // overlaps: previous hunk covers lines 1-2
                remove_lines: vec!["c".into()],
                insert_lines: vec!["y".into()],
            },
        ];
        let result = apply_hunks(original, &hunks, "test.rs");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("overlaps"), "got: {}", msg);
    }

    #[test]
    fn apply_hunks_preserves_no_trailing_newline() {
        let original = "line 1\nline 2"; // no trailing newline
        let hunks = vec![DiffHunk {
            original_start: 1,
            remove_lines: vec!["line 1".into()],
            insert_lines: vec!["replaced".into()],
        }];
        let result = apply_hunks(original, &hunks, "test.rs").unwrap();
        assert!(!result.ends_with('\n'));
        assert!(result.contains("replaced"));
    }

    #[test]
    fn apply_hunks_preserves_trailing_newline() {
        let original = "line 1\nline 2\n"; // has trailing newline
        let hunks = vec![DiffHunk {
            original_start: 1,
            remove_lines: vec!["line 1".into()],
            insert_lines: vec!["replaced".into()],
        }];
        let result = apply_hunks(original, &hunks, "test.rs").unwrap();
        assert!(result.ends_with('\n'));
        assert!(result.contains("replaced"));
    }

    #[test]
    fn apply_hunks_preserves_crlf_line_endings() {
        let original = "line 1\r\nline 2\r\nline 3\r\n";
        let hunks = vec![DiffHunk {
            original_start: 2,
            remove_lines: vec!["line 2".into()],
            insert_lines: vec!["replaced".into()],
        }];
        let result = apply_hunks(original, &hunks, "test.rs").unwrap();
        // Must preserve CRLF endings
        assert!(result.contains("\r\n"), "expected CRLF, got: {:?}", result);
        assert!(!result.contains("line 2"));
        assert!(result.contains("replaced\r\n"));
    }

    #[test]
    fn apply_hunks_elastic_trailing_whitespace() {
        // File has trailing spaces, LLM output doesn't — should still match
        let original = "fn foo()   \nfn bar()\n";
        let hunks = vec![DiffHunk {
            original_start: 1,
            remove_lines: vec!["fn foo()".into()], // no trailing spaces
            insert_lines: vec!["fn baz()".into()],
        }];
        let result = apply_hunks(original, &hunks, "test.rs").unwrap();
        assert!(result.contains("fn baz()"));
        assert!(!result.contains("fn foo()"));
    }

    #[test]
    fn serde_roundtrip_intent_plan() {
        let plan = IntentPlan {
            base_snapshot_id: crow_patch::SnapshotId("snap-1".into()),
            rationale: "test".into(),
            is_partial: false,
            confidence: crow_patch::Confidence::High,
            operations: vec![EditOp::Create {
                path: WorkspacePath::new("src/new.rs").unwrap(),
                content: "fn main() {}".into(),
                precondition: FilePrecondition::MustNotExist,
            }],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let restored: IntentPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, restored);
    }
}
