//! Diff-based file editing tool.
//!
//! Takes old_text/new_text pairs and performs surgical edits on files.
//! Dramatically reduces token usage compared to full-file replacement.
//! Includes:
//! - Path traversal guard (workspace boundary)
//! - Staleness detection (file modified since last read?)
//! - Unified diff output (shows exactly what changed)
//! - replace_all option for multi-occurrence edits
//! - Batch edits (apply N replacements atomically in one call)

use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use std::path::Path;

pub struct FileEditTool;

/// Validates that a path stays within the workspace root (no directory traversal).
pub(crate) fn validate_workspace_path(
    workspace_root: &Path,
    relative_path: &str,
) -> std::result::Result<std::path::PathBuf, ToolOutput> {
    let abs = workspace_root.join(relative_path);
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let canonical_target = if abs.exists() {
        abs.canonicalize().unwrap_or(abs.clone())
    } else if let Some(parent) = abs.parent() {
        let canonical_parent = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        canonical_parent.join(abs.file_name().unwrap_or_default())
    } else {
        abs.clone()
    };

    if !canonical_target.starts_with(&canonical_root) {
        return Err(ToolOutput::error(format!(
            "Path '{relative_path}' escapes workspace root. Only paths within the workspace are allowed."
        )));
    }
    Ok(canonical_target)
}

/// A single edit operation within a batch edit.
#[derive(serde::Deserialize, Clone, Debug)]
pub struct EditSpec {
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[async_trait::async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &'static str {
        "file_edit"
    }

    fn description(&self) -> &'static str {
        "Edit a file by replacing text. You MUST read the file first before editing. \
         The old_text must exactly match existing content (including whitespace and indentation).\n\
         Examples:\n\
         - file_edit(path='src/main.rs', old_text='println!(\"hello\")', new_text='println!(\"world\")') \
         — replace first occurrence\n\
         - file_edit(path='lib.rs', old_text='v1', new_text='v2', replace_all=true) \
         — replace all occurrences\n\
         - file_edit(path='lib.rs', edits=[{old_text:'a', new_text:'b'}, {old_text:'c', new_text:'d'}]) \
         — batch edit atomically\n\
         Batch mode is preferred for multiple edits to the same file."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to workspace root"
                },
                "old_text": {
                    "type": "string",
                    "description": "(single-edit mode) The exact text to find and replace. Must match the file content exactly."
                },
                "new_text": {
                    "type": "string",
                    "description": "(single-edit mode) The replacement text"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "(single-edit mode) If true, replace all occurrences. Default false (replace first only)."
                },
                "edits": {
                    "type": "array",
                    "description": "(batch mode) Array of {old_text, new_text, replace_all?} edits applied in order, atomically. When set, top-level old_text/new_text are ignored.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {"type": "string"},
                            "new_text": {"type": "string"},
                            "replace_all": {"type": "boolean"}
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            path: String,
            old_text: Option<String>,
            new_text: Option<String>,
            #[serde(default)]
            replace_all: bool,
            #[serde(default)]
            edits: Option<Vec<EditSpec>>,
        }
        let parsed: Args = serde_json::from_value(args)?;

        // Path traversal guard
        let abs_path = match validate_workspace_path(ctx.workspace_root, &parsed.path) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };

        // Permission check
        ctx.permissions.check_file_write(&abs_path)?;

        // ── Binary File Guard ────────────────────────────────────────
        if abs_path.exists() {
            match crate::file_safety::is_binary_file(&abs_path) {
                Ok(true) => {
                    return Ok(ToolOutput::error(format!(
                        "File '{}' appears to be binary. Cannot apply text edits to binary files.",
                        parsed.path
                    )));
                }
                Err(e) => {
                    return Ok(ToolOutput::error(format!(
                        "Cannot check file type of '{}': {e}",
                        parsed.path
                    )));
                }
                Ok(false) => {}
            }
        }

        // Read current content first so we can hash it for staleness detection
        let original_content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "Cannot read file '{}': {e}. Make sure you've read the file first with read_file.",
                    parsed.path
                )));
            }
        };

        // Staleness check (hash-augmented — catches sub-second edits that
        // mtime resolution can miss).
        if let Some(ref store) = ctx.file_state {
            if !store.has_recorded(&abs_path) {
                return Ok(ToolOutput::error(format!(
                    "File '{}' has not been read yet. Read it first before editing.",
                    parsed.path
                )));
            }
            let current_mtime = crate::file_state::get_file_mtime(&abs_path).await;
            let current_hash = crate::file_state::hash_content(original_content.as_bytes());
            if store.is_stale_with_hash(&abs_path, current_mtime, current_hash) {
                return Ok(ToolOutput::error(format!(
                    "File '{}' has been modified since it was last read. Read the file again before editing.",
                    parsed.path
                )));
            }
        }

        // ── Resolve edit list ───────────────────────────────────────
        let resolved_edits: Vec<EditSpec> = if let Some(batch) = parsed.edits {
            if batch.is_empty() {
                return Ok(ToolOutput::error(
                    "edits array is empty. Provide at least one edit, or omit edits and use single-edit mode.",
                ));
            }
            batch
        } else {
            // Single-edit mode — validate fields
            let Some(old) = parsed.old_text else {
                return Ok(ToolOutput::error(
                    "Missing old_text. Provide old_text/new_text for single edit, or use the `edits` array for batch edits.",
                ));
            };
            let Some(new) = parsed.new_text else {
                return Ok(ToolOutput::error(
                    "Missing new_text. Provide old_text/new_text for single edit, or use the `edits` array for batch edits.",
                ));
            };
            vec![EditSpec {
                old_text: old,
                new_text: new,
                replace_all: parsed.replace_all,
            }]
        };

        // ── Apply all edits in-memory before writing ────────────────
        let mut working = original_content.clone();
        let mut total_replacements = 0usize;
        for (i, spec) in resolved_edits.iter().enumerate() {
            // Validate spec
            if spec.old_text.is_empty() && !working.is_empty() {
                return Ok(ToolOutput::error(format!(
                    "Edit #{}: cannot use empty old_text on non-empty file. Provide the text to replace.",
                    i + 1
                )));
            }
            if spec.old_text == spec.new_text {
                return Ok(ToolOutput::error(format!(
                    "Edit #{}: no changes to make — old_text and new_text are identical.",
                    i + 1
                )));
            }

            let count = working.matches(&spec.old_text).count();
            if count == 0 {
                return Ok(ToolOutput::error(format!(
                    "Edit #{}: old_text not found in '{}'. Earlier edits in this batch may have already changed this region. Read the file again to see current state. (No edits committed.)",
                    i + 1,
                    parsed.path
                )));
            }
            if count > 1 && !spec.replace_all {
                return Ok(ToolOutput::error(format!(
                    "Edit #{}: old_text found {count} times in '{}'. Set replace_all=true on this edit, or provide more surrounding context to uniquely identify the instance. (No edits committed.)",
                    i + 1,
                    parsed.path
                )));
            }

            working = if spec.replace_all {
                total_replacements += count;
                working.replace(&spec.old_text, &spec.new_text)
            } else {
                total_replacements += 1;
                working.replacen(&spec.old_text, &spec.new_text, 1)
            };
        }

        // Nothing to write?
        if working == original_content {
            return Ok(ToolOutput::error(
                "No changes resulted from the edits.",
            ));
        }

        // Write back
        if let Err(e) = std::fs::write(&abs_path, &working) {
            return Ok(ToolOutput::error(format!(
                "Failed to write file '{path}': {e}",
                path = parsed.path
            )));
        }

        // Update file state tracking with new mtime + content hash
        if let Some(ref store) = ctx.file_state {
            let mtime = crate::file_state::get_file_mtime(&abs_path).await;
            let new_hash = crate::file_state::hash_content(working.as_bytes());
            store.record_with_hash(abs_path, mtime, new_hash);
        }

        // Generate unified diff for response
        let diff = crate::diff_utils::generate_diff(&original_content, &working, 3);
        let summary = crate::diff_utils::diff_summary(&original_content, &working);

        let action = if resolved_edits.len() == 1 {
            if total_replacements > 1 {
                format!("Replaced all {total_replacements} occurrences")
            } else {
                "Replaced 1 occurrence".to_string()
            }
        } else {
            format!(
                "Applied {} edit(s) atomically ({total_replacements} total replacements)",
                resolved_edits.len()
            )
        };

        Ok(ToolOutput::success(format!(
            "{action} in {} ({summary})\n\n{diff}",
            parsed.path
        )))
    }
}
