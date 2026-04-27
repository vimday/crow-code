//! Diff-based file editing tool.
//!
//! Takes old_text/new_text pairs and performs surgical edits on files.
//! Dramatically reduces token usage compared to full-file replacement.
//! Includes:
//! - Path traversal guard (workspace boundary)
//! - Staleness detection (file modified since last read?)
//! - Unified diff output (shows exactly what changed)
//! - replace_all option for multi-occurrence edits

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

#[async_trait::async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &'static str {
        "file_edit"
    }

    fn description(&self) -> &'static str {
        "Edit a file by replacing specific text. You MUST read the file first before editing. \
         The old_text must exactly match existing content in the file (including whitespace and indentation). \
         For multiple edits in the same file, make one call per edit. \
         Supports replace_all=true to replace all occurrences."
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
                    "description": "The exact text to find and replace. Must match the file content exactly."
                },
                "new_text": {
                    "type": "string",
                    "description": "The replacement text"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences. Default false (replace first only)."
                }
            },
            "required": ["path", "old_text", "new_text"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            path: String,
            old_text: String,
            new_text: String,
            #[serde(default)]
            replace_all: bool,
        }
        let parsed: Args = serde_json::from_value(args)?;

        // Path traversal guard
        let abs_path = match validate_workspace_path(ctx.workspace_root, &parsed.path) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };

        // Permission check
        ctx.permissions.check_file_write(&abs_path)?;

        // ── Binary File Guard (claw-code pattern) ─────────────────────
        // Prevent text-replacement edits on binary files, which would
        // silently corrupt them.
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
                Ok(false) => {} // text file, proceed
            }
        }

        // Staleness check (if file state tracking is available)
        if let Some(ref store) = ctx.file_state {
            if !store.has_recorded(&abs_path) {
                return Ok(ToolOutput::error(format!(
                    "File '{}' has not been read yet. Read it first before editing.",
                    parsed.path
                )));
            }
            let current_mtime = crate::file_state::get_file_mtime(&abs_path).await;
            if store.is_stale(&abs_path, current_mtime) {
                return Ok(ToolOutput::error(format!(
                    "File '{}' has been modified since it was last read. Read the file again before editing.",
                    parsed.path
                )));
            }
        }

        // Read current content
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "Cannot read file '{}': {e}. Make sure you've read the file first with read_file.",
                    parsed.path
                )));
            }
        };

        // Validate old_text is not empty on non-empty file
        if parsed.old_text.is_empty() && !content.is_empty() {
            return Ok(ToolOutput::error(
                "Cannot use empty old_text on existing file with content. Provide the text to replace."
            ));
        }

        // Check old_text == new_text
        if parsed.old_text == parsed.new_text {
            return Ok(ToolOutput::error(
                "No changes to make: old_text and new_text are exactly the same.",
            ));
        }

        // Find and replace
        let count = content.matches(&parsed.old_text).count();
        if count == 0 {
            return Ok(ToolOutput::error(format!(
                "old_text not found in '{}'. The text must exactly match existing content \
                 (including whitespace). Read the file again to see current content.",
                parsed.path
            )));
        }
        if count > 1 && !parsed.replace_all {
            return Ok(ToolOutput::error(format!(
                "old_text found {count} times in '{}'. Set replace_all=true to replace all, \
                 or provide more context to uniquely identify the instance.",
                parsed.path
            )));
        }

        let new_content = if parsed.replace_all {
            content.replace(&parsed.old_text, &parsed.new_text)
        } else {
            content.replacen(&parsed.old_text, &parsed.new_text, 1)
        };

        // Write back
        if let Err(e) = std::fs::write(&abs_path, &new_content) {
            return Ok(ToolOutput::error(format!(
                "Failed to write file '{path}': {e}",
                path = parsed.path
            )));
        }

        // Update file state tracking
        if let Some(ref store) = ctx.file_state {
            let mtime = crate::file_state::get_file_mtime(&abs_path).await;
            store.record(abs_path, mtime);
        }

        // Generate unified diff for response
        let diff = crate::diff_utils::generate_diff(&content, &new_content, 3);
        let summary = crate::diff_utils::diff_summary(&content, &new_content);

        let action = if parsed.replace_all && count > 1 {
            format!("Replaced all {count} occurrences")
        } else {
            "Replaced 1 occurrence".to_string()
        };

        Ok(ToolOutput::success(format!(
            "{action} in {} ({summary})\n\n{diff}",
            parsed.path
        )))
    }
}
