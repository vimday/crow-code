//! Full-file write tool for creating or overwriting files.
//!
//! Includes:
//! - Path traversal guard (workspace boundary)
//! - Staleness detection for existing files
//! - Unified diff output for overwrites
//! - Automatic parent directory creation

use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;

pub struct FileWriteTool;

#[async_trait::async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &'static str {
        "file_write"
    }

    fn description(&self) -> &'static str {
        "Create a new file or overwrite an existing file with the given content. \
         Parent directories are created automatically. For modifying existing files, \
         prefer file_edit instead. If the file already exists, you must read it first."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to workspace root"
                },
                "content": {
                    "type": "string",
                    "description": "Full file content to write"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            path: String,
            content: String,
        }
        let parsed: Args = serde_json::from_value(args)?;

        // Path traversal guard
        let abs_path = match crate::file_edit::validate_workspace_path(ctx.workspace_root, &parsed.path) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };

        // Permission check
        ctx.permissions.check_file_write(&abs_path)?;

        let file_exists = abs_path.exists();

        // Staleness check for existing files
        if file_exists {
            if let Some(ref store) = ctx.file_state {
                if !store.has_recorded(&abs_path) {
                    return Ok(ToolOutput::error(format!(
                        "File '{}' exists but has not been read yet. Read it first before overwriting.",
                        parsed.path
                    )));
                }
                let current_mtime = crate::file_state::get_file_mtime(&abs_path).await;
                if store.is_stale(&abs_path, current_mtime) {
                    return Ok(ToolOutput::error(format!(
                        "File '{}' has been modified since it was last read. Read the file again before writing.",
                        parsed.path
                    )));
                }
            }
        }

        // Read original content for diff (if overwriting)
        let original_content = if file_exists {
            std::fs::read_to_string(&abs_path).ok()
        } else {
            None
        };

        // Create parent directories
        if let Some(parent) = abs_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Ok(ToolOutput::error(format!(
                    "Failed to create directories for '{}': {e}",
                    parsed.path
                )));
            }
        }

        // Write file
        if let Err(e) = std::fs::write(&abs_path, &parsed.content) {
            return Ok(ToolOutput::error(format!(
                "Failed to write file '{}': {e}",
                parsed.path
            )));
        }

        // Update file state tracking
        if let Some(ref store) = ctx.file_state {
            let mtime = crate::file_state::get_file_mtime(&abs_path).await;
            store.record(abs_path, mtime);
        }

        // Build response
        let line_count = parsed.content.lines().count();
        let byte_count = parsed.content.len();

        if let Some(ref old_content) = original_content {
            // File was overwritten — show diff
            let diff = crate::diff_utils::generate_diff(old_content, &parsed.content, 3);
            let summary = crate::diff_utils::diff_summary(old_content, &parsed.content);
            Ok(ToolOutput::success(format!(
                "Updated {}: {line_count} lines, {byte_count} bytes ({summary})\n\n{diff}",
                parsed.path
            )))
        } else {
            // New file created
            Ok(ToolOutput::success(format!(
                "Created {}: {line_count} lines, {byte_count} bytes",
                parsed.path
            )))
        }
    }
}
