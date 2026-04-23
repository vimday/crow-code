//! Diff-based file editing tool.
//!
//! Takes old_text/new_text pairs and performs surgical edits on files.
//! Dramatically reduces token usage compared to full-file replacement.
//! Includes built-in hallucination guard: the file must have been read first.

use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use std::path::Path;

pub struct FileEditTool;

/// Validates that a path stays within the workspace root (no directory traversal).
pub(crate) fn validate_workspace_path(workspace_root: &Path, relative_path: &str) -> Result<std::path::PathBuf, ToolOutput> {
    let abs = workspace_root.join(relative_path);
    // Canonicalize the workspace root and the target path to resolve symlinks and ..
    let canonical_root = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());
    // For the target, we canonicalize the parent (which must exist) and join the filename
    let canonical_target = if abs.exists() {
        abs.canonicalize().unwrap_or(abs.clone())
    } else if let Some(parent) = abs.parent() {
        let canonical_parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
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
         For multiple edits in the same file, make one call per edit."
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
        }
        let parsed: Args = serde_json::from_value(args)?;

        // Path traversal guard
        let abs_path = match validate_workspace_path(ctx.frozen_root, &parsed.path) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };

        // Permission check
        ctx.permissions.check_file_write(&abs_path)?;

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

        // Find and replace
        let count = content.matches(&parsed.old_text).count();
        if count == 0 {
            return Ok(ToolOutput::error(format!(
                "old_text not found in '{}'. The text must exactly match existing content \
                 (including whitespace). Read the file again to see current content.",
                parsed.path
            )));
        }
        if count > 1 {
            return Ok(ToolOutput::error(format!(
                "old_text found {} times in '{}'. Provide a more specific old_text that matches \
                 exactly once. Include more surrounding context to disambiguate.",
                count, parsed.path
            )));
        }

        let new_content = content.replacen(&parsed.old_text, &parsed.new_text, 1);

        // Write back
        if let Err(e) = std::fs::write(&abs_path, &new_content) {
            return Ok(ToolOutput::error(format!("Failed to write file '{path}': {e}", path = parsed.path)));
        }

        let old_lines = parsed.old_text.lines().count();
        let new_lines = parsed.new_text.lines().count();
        Ok(ToolOutput::success(format!(
            "Edited {}: replaced {} line(s) with {} line(s)",
            parsed.path, old_lines, new_lines
        )))
    }
}
