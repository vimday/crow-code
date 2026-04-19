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

/// Delegates to the canonical implementation in `crow_patch::sha256_hex`.
fn sha256_hex(data: &[u8]) -> String {
    crow_patch::sha256_hex(data)
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

/// Apply a sequence of hunks using a conservative fuzzy matcher.
///
/// Key properties:
/// - Hunks are applied from bottom to top, so lower-file edits never
///   perturb the anchoring of earlier hunks.
/// - Matching is elastic with respect to leading and trailing whitespace,
///   tolerating common LLM indentation hallucinations during anchoring.
/// - Each hunk may drift within a small bounded window (±10 lines).
/// - Multiple matches inside that window are treated as ambiguous and
///   rejected rather than guessed.
/// - The original line-ending style (LF vs CRLF) and trailing-newline
///   state are preserved.
fn apply_hunks(original: &str, hunks: &[DiffHunk], file_path: &str) -> Result<String, ApplyError> {
    if hunks.is_empty() {
        return Ok(original.to_string());
    }

    let line_ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let trailing_newline = original.ends_with('\n') || original.is_empty();
    let mut lines: Vec<String> = original.lines().map(String::from).collect();

    let mut sorted_hunks = hunks.to_vec();
    sorted_hunks.sort_by(|a, b| b.original_start.cmp(&a.original_start));

    // Convert strings back into physical line arrays for precise manipulation
    #[derive(Clone)]
    struct ProcessedHunk<'a> {
        original_start: usize,
        remove_lines: Vec<&'a str>,
        insert_lines: Vec<&'a str>,
    }

    let p_hunks: Vec<ProcessedHunk> = sorted_hunks
        .iter()
        .map(|h| {
            ProcessedHunk {
                original_start: h.original_start,
                // Splitting by \n properly preserves empty internal lines.
                // If the block is completely empty, it drops no lines.
                remove_lines: if h.remove_block.is_empty() {
                    vec![]
                } else {
                    h.remove_block.lines().collect()
                },
                insert_lines: if h.insert_block.is_empty() {
                    vec![]
                } else {
                    h.insert_block.lines().collect()
                },
            }
        })
        .collect();

    // ── Structural validation ──────────────────────────────────
    for (i, hunk) in p_hunks.iter().enumerate() {
        if hunk.original_start == 0 {
            return Err(ApplyError::HunkConflict {
                path: file_path.into(),
                line: 0,
                reason: "original_start must be 1-based, got 0".into(),
            });
        }
        if i > 0 {
            let bottom_hunk = &p_hunks[i - 1];
            let top_end = hunk.original_start + hunk.remove_lines.len();
            if top_end > bottom_hunk.original_start {
                return Err(ApplyError::HunkConflict {
                    path: file_path.into(),
                    line: hunk.original_start,
                    reason: format!(
                        "hunks must be non-overlapping; hunk at {} overlaps hunk at {}",
                        hunk.original_start, bottom_hunk.original_start
                    ),
                });
            }
        }
    }

    let max_drift: isize = 10;

    for hunk in p_hunks {
        let expected_idx = (hunk.original_start as isize - 1).max(0);
        let target_len = hunk.remove_lines.len();
        let mut match_indices = Vec::new();

        if target_len == 0 {
            // Pure insertion with no context anchor is forbidden.
            // The LLM must include at least one existing line in remove_block
            // (and replicate it in insert_block alongside the new lines) so
            // that the drift search can verify the correct insertion site.
            return Err(ApplyError::HunkConflict {
                path: file_path.into(),
                line: hunk.original_start,
                reason: "contextless pure insertion rejected: remove_block is empty. \
                         Include at least one existing line as anchor context in remove_block \
                         and repeat it in insert_block alongside the new lines."
                    .into(),
            });
        } else {
            for drift in -max_drift..=max_drift {
                let probe_idx = expected_idx + drift;
                if probe_idx < 0 || (probe_idx as usize) + target_len > lines.len() {
                    continue;
                }

                let probe_idx = probe_idx as usize;
                let mut is_match = true;
                for (i, expected_line) in hunk.remove_lines.iter().enumerate() {
                    // Use trim() (both ends) so that LLM indentation
                    // hallucinations (e.g. 4-space vs 8-space) don't waste
                    // a retry. The probe only needs semantic identity;
                    // the actual insert uses the LLM's exact output.
                    if lines[probe_idx + i].trim() != expected_line.trim() {
                        is_match = false;
                        break;
                    }
                }

                if is_match {
                    match_indices.push(probe_idx);
                }
            }
        }

        match match_indices.len() {
            1 => {
                let start_idx = match_indices[0];
                lines.splice(
                    start_idx..start_idx + target_len,
                    hunk.insert_lines.iter().map(|s| s.to_string()),
                );
            }
            0 => {
                let context = hunk.remove_lines.first().copied().unwrap_or("<empty>");
                return Err(ApplyError::HunkConflict {
                    path: file_path.into(),
                    line: hunk.original_start,
                    reason: format!(
                        "context not found within ±{} lines ('{}').",
                        max_drift, context
                    ),
                });
            }
            _ => {
                return Err(ApplyError::HunkConflict {
                    path: file_path.into(),
                    line: hunk.original_start,
                    reason: format!(
                        "ambiguous match: found {} identical contexts within ±{} lines. Refusing to guess.",
                        match_indices.len(),
                        max_drift
                    ),
                });
            }
        }
    }

    let mut result = lines.join(line_ending);
    if trailing_newline && !result.is_empty() {
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

/// Apply verified changes from sandbox back to the real workspace.
///
/// Only copies files that were changed by the plan (not the entire sandbox).
/// This ensures minimal filesystem mutation and respects the zero-pollution
/// invariant: if anything goes wrong, the workspace is untouched.
pub fn apply_sandbox_to_workspace(
    workspace_root: &std::path::Path,
    plan: &crow_patch::IntentPlan,
) -> Result<(), anyhow::Error> {
    struct RollbackRecord {
        dst_path: std::path::PathBuf,
        original_content: Option<Vec<u8>>,
    }
    let mut rollback_log: Vec<RollbackRecord> = Vec::new();
    let mut created_dirs: Vec<std::path::PathBuf> = Vec::new();

    // Phase 1: Snapshot original states for rollback
    for op in &plan.operations {
        let (dst, from_dst) = match op {
            EditOp::Create { path, .. }
            | EditOp::Modify { path, .. }
            | EditOp::Delete { path, .. } => (path.to_absolute(workspace_root), None),
            EditOp::Rename { from, to, .. } => (
                to.to_absolute(workspace_root),
                Some(from.to_absolute(workspace_root)),
            ),
        };

        for path_to_eval in std::iter::once(&Some(dst.clone()))
            .chain(std::iter::once(&from_dst))
            .flatten()
        {
            if path_to_eval.is_symlink()
                || (path_to_eval.exists()
                    && !path_to_eval
                        .canonicalize()
                        .unwrap_or_else(|_| path_to_eval.clone())
                        .starts_with(workspace_root))
            {
                anyhow::bail!("Security violation: operation attempts to modify a symlink or an external path: {}", path_to_eval.display());
            }
        }

        let original_content = if dst.exists() && dst.is_file() {
            std::fs::read(&dst).ok()
        } else {
            None
        };
        rollback_log.push(RollbackRecord {
            dst_path: dst.clone(),
            original_content,
        });

        if let Some(ref fdst) = from_dst {
            let original_from = if fdst.exists() && fdst.is_file() {
                std::fs::read(fdst).ok()
            } else {
                None
            };
            rollback_log.push(RollbackRecord {
                dst_path: fdst.clone(),
                original_content: original_from,
            });
        }

        let mut track_new_dir = |p: &std::path::Path| {
            if let Some(mut current) = p.parent() {
                let mut highest_new = None;
                while !current.exists() {
                    highest_new = Some(current.to_path_buf());
                    if let Some(parent) = current.parent() {
                        current = parent;
                    } else {
                        break;
                    }
                }
                if let Some(h) = highest_new {
                    if !created_dirs.contains(&h) {
                        created_dirs.push(h);
                    }
                }
            }
        };

        track_new_dir(&dst);
        if let Some(ref f) = from_dst {
            track_new_dir(f);
        }
    }

    // Phase 2: Attempt destructive apply via proper unified applier logic
    let mut apply_failed = false;
    let mut apply_error = String::new();

    match crate::PlanHydrator::hydrate(plan, &plan.base_snapshot_id, workspace_root) {
        Ok(hydrated_workspace_plan) => {
            let live_view = crow_materialize::SandboxHandle::non_owning_view_from(
                workspace_root.to_path_buf(),
                crow_materialize::MaterializationDriver::SafeCopy,
            );
            if let Err(e) = apply_plan_to_sandbox(&hydrated_workspace_plan, &live_view) {
                apply_failed = true;
                apply_error = format!("Workspace Applier Failed: {}", e);
            }
        }
        Err(e) => {
            apply_failed = true;
            apply_error = format!("Failed to hydrate plan for live workspace: {}", e);
        }
    }

    if apply_failed {
        eprintln!("\n🚨 Apply failed mid-flight. Executing zero-pollution rollback...");
        for record in rollback_log {
            if let Some(content) = record.original_content {
                if let Some(parent) = record.dst_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&record.dst_path, content);
            } else if record.dst_path.exists() {
                let _ = std::fs::remove_file(&record.dst_path);
            }
        }
        for dir in created_dirs {
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        anyhow::bail!("Transaction failed and rolled back. Cause: {}", apply_error);
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
    fn apply_hunks_rejects_contextless_pure_insertion() {
        let original = "line 1\nline 2\nline 3\n";
        let hunks = vec![DiffHunk {
            original_start: 2,
            remove_block: "".into(),
            insert_block: "inserted\n".into(),
        }];
        let result = apply_hunks(original, &hunks, "test.rs");
        assert!(
            result.is_err(),
            "contextless pure insertion must be rejected"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("contextless pure insertion"), "got: {}", msg);
    }

    #[test]
    fn apply_hunks_anchored_insertion() {
        // The correct way: include anchor context in remove_block,
        // replicate it plus new lines in insert_block.
        let original = "line 1\nline 2\nline 3\n";
        let hunks = vec![DiffHunk {
            original_start: 2,
            remove_block: "line 2\n".into(),
            insert_block: "line 2\ninserted\n".into(),
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
            remove_block: "line 2\n".into(),
            insert_block: "".into(),
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
            remove_block: "fn old() {}\n".into(),
            insert_block: "fn new() {}\n".into(),
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
            remove_block: "WRONG CONTEXT\n".into(),
            insert_block: "new\n".into(),
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
            remove_block: "line 1\n".into(),
            insert_block: "bad\n".into(),
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
                remove_block: "a\nb\n".into(),
                insert_block: "x\n".into(),
            },
            DiffHunk {
                original_start: 2, // overlaps: previous hunk covers lines 1-2
                remove_block: "c\n".into(),
                insert_block: "y\n".into(),
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
            remove_block: "line 1\n".into(),
            insert_block: "replaced\n".into(),
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
            remove_block: "line 1\n".into(),
            insert_block: "replaced\n".into(),
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
            remove_block: "line 2\n".into(),
            insert_block: "replaced\n".into(),
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
            remove_block: "fn foo()\n".into(), // no trailing spaces
            insert_block: "fn baz()\n".into(),
        }];
        let result = apply_hunks(original, &hunks, "test.rs").unwrap();
        assert!(result.contains("fn baz()"));
        assert!(!result.contains("fn foo()"));
    }

    #[test]
    fn apply_hunks_finds_context_with_small_line_drift() {
        let original = "line 1\nline 2\nline 3\nline 4\nunique target\nline 6\n";
        let hunks = vec![DiffHunk {
            original_start: 3, // true line is 5, but within ±10 drift
            remove_block: "unique target\n".into(),
            insert_block: "patched target\n".into(),
        }];

        let result = apply_hunks(original, &hunks, "test.rs").unwrap();
        assert!(result.contains("patched target"));
        assert!(!result.contains("unique target"));
    }

    #[test]
    fn apply_hunks_rejects_ambiguous_drift_matches() {
        let original = "line 1\nrepeated target\nline 3\nline 4\nrepeated target\nline 6\n";
        let hunks = vec![DiffHunk {
            original_start: 3,
            remove_block: "repeated target\n".into(),
            insert_block: "patched target\n".into(),
        }];

        let result = apply_hunks(original, &hunks, "test.rs");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("ambiguous match"), "got: {}", msg);
    }

    #[test]
    fn serde_roundtrip_intent_plan() {
        let plan = IntentPlan {
            base_snapshot_id: crow_patch::SnapshotId("snap-1".into()),
            rationale: "test".into(),
            is_partial: false,
            confidence: crow_patch::Confidence::High,
            requires_mcts: true,
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
