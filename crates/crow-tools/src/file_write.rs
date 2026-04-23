//! Full-file write tool for creating new files.
//!
//! Creates new files (or overwrites existing ones).
//! Automatically creates parent directories.
//! Includes path traversal guard to prevent writing outside workspace.

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
         prefer file_edit instead."
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

        // Path traversal guard — reuse the same validation as file_edit
        let abs_path = match crate::file_edit::validate_workspace_path(ctx.frozen_root, &parsed.path) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };

        // Permission check
        ctx.permissions.check_file_write(&abs_path)?;

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

        let line_count = parsed.content.lines().count();
        let byte_count = parsed.content.len();
        Ok(ToolOutput::success(format!(
            "Created {}: {} lines, {} bytes",
            parsed.path, line_count, byte_count
        )))
    }
}
