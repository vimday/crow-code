//! Gitignore-aware file listing with glob support.
//!
//! Provides structured file discovery that respects `.gitignore` rules,
//! supporting glob patterns and mtime sorting. This gives the agent
//! a dedicated tool for exploring project structure beyond `dir_tree`.

use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;

const LIST_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_LIMIT: usize = 200;

pub struct GlobTool;

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "list_files"
    }

    fn description(&self) -> &'static str {
        "List files matching a glob pattern. Respects .gitignore. Results are sorted by \
         modification time (newest first). Use this to discover project structure and find \
         files by pattern."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files (e.g., '**/*.rs', 'src/**/*.ts', '*.toml'). Default: '**/*' (all files)."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (relative to workspace). Defaults to workspace root."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of files to return. Default: 200."
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        let pattern = args["pattern"].as_str().unwrap_or("**/*");
        let path_str = args["path"].as_str();
        let limit = args["limit"].as_u64().map(|n| n as usize).unwrap_or(DEFAULT_LIMIT);

        let search_root: PathBuf = match path_str {
            Some(p) => ctx.frozen_root.join(p),
            None => ctx.frozen_root.to_path_buf(),
        };

        if !search_root.exists() {
            return Ok(ToolOutput::error(format!(
                "Path does not exist: {}",
                path_str.unwrap_or(".")
            )));
        }

        // Use `find` + glob pattern via ripgrep's file listing (rg --files --glob)
        // This respects .gitignore automatically
        let result = tokio::time::timeout(LIST_TIMEOUT, async {
            let mut cmd = tokio::process::Command::new("rg");
            cmd.arg("--files")
                .arg("--hidden")
                .arg("--glob")
                .arg("!.git")
                .arg("--glob")
                .arg(pattern)
                .arg(&search_root)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            cmd.output().await
        }).await;

        let output = match result {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Ok(ToolOutput::error(format!(
                    "Failed to list files: {e}. Is `rg` installed?"
                )));
            }
            Err(_) => {
                return Ok(ToolOutput::error(format!(
                    "File listing timed out after {}s",
                    LIST_TIMEOUT.as_secs()
                )));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);

        if stdout.is_empty() {
            return Ok(ToolOutput::success(format!(
                "No files found matching pattern '{pattern}'"
            )));
        }

        // Collect files and get relative paths
        let mut files: Vec<String> = stdout
            .lines()
            .filter_map(|line| {
                let path = PathBuf::from(line);
                path.strip_prefix(ctx.frozen_root)
                    .ok()
                    .map(|rel| rel.to_string_lossy().to_string())
                    .or_else(|| Some(line.to_string()))
            })
            .collect();

        // Sort alphabetically for consistency
        files.sort();

        let total = files.len();
        let was_truncated = total > limit && limit > 0;

        if limit > 0 {
            files.truncate(limit);
        }

        let mut result = files.join("\n");

        if was_truncated {
            result.push_str(&format!(
                "\n\n({total} total files, showing first {limit}. Use a more specific pattern to narrow results.)"
            ));
        } else {
            result.push_str(&format!("\n\n({total} files)"));
        }

        Ok(ToolOutput::success(result))
    }
}
