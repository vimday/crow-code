//! Native glob file search tool.
//!
//! Ported from yomi's `tools/glob.rs`. Uses the `ignore` crate for
//! `.gitignore`-respecting directory walking and `globset` for pattern
//! matching. Eliminates the need to shell out to `find` or `fd`.
//!
//! Features:
//! - Respects `.gitignore` by default
//! - Sorts results by mtime (newest first)
//! - Limits to MAX_RESULTS to avoid overwhelming the context window
//! - Supports brace expansion (`*.{rs,ts,js}`)

use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use std::path::PathBuf;

/// Maximum number of glob results to return.
const MAX_RESULTS: usize = 100;

pub struct GlobTool;

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn description(&self) -> &'static str {
        "Find files matching a glob pattern. Supports patterns like '**/*.rs' or \
         'src/**/*.ts'. Respects .gitignore by default. Results sorted by \
         modification time (newest first), limited to 100 results."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match (e.g., '**/*.rs', 'src/**/*.{ts,tsx}')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (relative to workspace root). Defaults to workspace root."
                },
                "include_ignored": {
                    "type": "boolean",
                    "description": "Include files ignored by .gitignore. Default: false"
                },
                "include_hidden": {
                    "type": "boolean",
                    "description": "Include hidden files (starting with .). Default: true"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            pattern: String,
            path: Option<String>,
            #[serde(default)]
            include_ignored: bool,
            #[serde(default = "default_true")]
            include_hidden: bool,
        }
        fn default_true() -> bool {
            true
        }
        let parsed: Args = serde_json::from_value(args)?;

        // Determine search directory
        let search_dir = match parsed.path {
            Some(ref p) => ctx.workspace_root.join(p),
            None => ctx.workspace_root.to_path_buf(),
        };

        // Validate directory exists
        if !search_dir.exists() {
            return Ok(ToolOutput::error(format!(
                "Directory does not exist: {}",
                parsed.path.as_deref().unwrap_or(".")
            )));
        }
        if !search_dir.is_dir() {
            return Ok(ToolOutput::error(format!(
                "Path is not a directory: {}",
                parsed.path.as_deref().unwrap_or(".")
            )));
        }

        // Build glob matcher
        let matcher = if parsed.pattern.is_empty() || parsed.pattern == "**/*" {
            None
        } else {
            let glob = globset::Glob::new(&parsed.pattern)
                .map_err(|e| anyhow::anyhow!("Invalid glob pattern '{}': {e}", parsed.pattern))?;
            Some(glob.compile_matcher())
        };

        // Search files using ignore crate (respects .gitignore)
        let include_ignored = parsed.include_ignored;
        let include_hidden = parsed.include_hidden;
        let search_dir_clone = search_dir.clone();

        let files = tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();

            let walker = ignore::WalkBuilder::new(&search_dir_clone)
                .standard_filters(!include_ignored)
                .hidden(!include_hidden)
                .follow_links(false)
                .build();

            for entry in walker.flatten() {
                if let Some(ft) = entry.file_type() {
                    if ft.is_file() {
                        let path = entry.path();

                        // Apply glob pattern matching
                        if let Some(ref m) = matcher {
                            let relative = path
                                .strip_prefix(&search_dir_clone)
                                .unwrap_or(path)
                                .to_string_lossy();
                            if !m.is_match(&*relative) {
                                continue;
                            }
                        }

                        files.push(path.to_path_buf());
                    }
                }
            }

            files
        })
        .await
        .map_err(|e| anyhow::anyhow!("Glob task error: {e}"))?;

        // Get mtimes and sort by newest first
        let mut files_with_mtime: Vec<(PathBuf, u64)> = files
            .into_iter()
            .map(|path| {
                let mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as u64);
                (path, mtime)
            })
            .collect();

        files_with_mtime.sort_by(|a, b| {
            b.1.cmp(&a.1) // Descending by mtime
                .then_with(|| a.0.cmp(&b.0)) // Ascending by path as tiebreaker
        });

        let total = files_with_mtime.len();
        let truncated = total > MAX_RESULTS;

        // Convert to relative paths
        let filenames: Vec<String> = files_with_mtime
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(path, _)| {
                path.strip_prefix(ctx.workspace_root).map_or_else(
                    |_| path.to_string_lossy().to_string(),
                    |p| p.to_string_lossy().to_string(),
                )
            })
            .collect();

        // Build response
        if filenames.is_empty() {
            return Ok(ToolOutput::success(
                "No files found matching the pattern.".to_string(),
            ));
        }

        let mut response = filenames.join("\n");

        if truncated {
            response.push_str(&format!(
                "\n\n[Showing {MAX_RESULTS} of {total} results. Use a more specific path or pattern.]"
            ));
        }

        response.insert_str(
            0,
            &format!(
                "Found {total} file{}\n\n",
                if total == 1 { "" } else { "s" }
            ),
        );

        Ok(ToolOutput::success(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_ctx(path: &std::path::Path) -> ToolContext<'_> {
        // We need a 'static lifetime for the permissions reference.
        // In tests, leak a Box to get a &'static reference.
        let permissions: &'static crate::PermissionEnforcer = Box::leak(Box::new(
            crate::PermissionEnforcer::new(crate::WriteMode::Sandbox),
        ));
        ToolContext {
            workspace_root: path,
            permissions,
            file_state: None,
            background_manager: None,
            subagent_delegator: None,
        }
    }

    #[tokio::test]
    async fn finds_rust_files() {
        let temp = TempDir::new().expect("temp dir");
        let base = temp.path();

        let mut f1 = std::fs::File::create(base.join("main.rs")).expect("create");
        writeln!(f1, "fn main() {{}}").expect("write");
        let mut f2 = std::fs::File::create(base.join("lib.rs")).expect("create");
        writeln!(f2, "pub mod foo;").expect("write");
        std::fs::File::create(base.join("readme.md")).expect("create");

        let tool = GlobTool;
        let args = serde_json::json!({ "pattern": "*.rs" });
        let ctx = make_ctx(base);
        let result = tool.execute(args, &ctx).await.expect("execute");

        let ToolOutput { content, .. } = &result;
        let text = content.clone();
        assert!(text.contains("main.rs"), "Should find main.rs");
        assert!(text.contains("lib.rs"), "Should find lib.rs");
        assert!(!text.contains("readme.md"), "Should not find readme.md");
    }

    #[tokio::test]
    async fn recursive_search() {
        let temp = TempDir::new().expect("temp dir");
        let base = temp.path();
        let sub = base.join("src");
        std::fs::create_dir(&sub).expect("mkdir");

        let mut f = std::fs::File::create(sub.join("deep.rs")).expect("create");
        writeln!(f, "mod deep;").expect("write");

        let tool = GlobTool;
        let args = serde_json::json!({ "pattern": "**/*.rs" });
        let ctx = make_ctx(base);
        let result = tool.execute(args, &ctx).await.expect("execute");

        assert!(result.content.contains("src/deep.rs"));
    }

    #[tokio::test]
    async fn no_matches() {
        let temp = TempDir::new().expect("temp dir");
        let tool = GlobTool;
        let args = serde_json::json!({ "pattern": "*.nonexistent" });
        let ctx = make_ctx(temp.path());
        let result = tool.execute(args, &ctx).await.expect("execute");
        assert!(result.content.contains("No files found"));
    }

    #[tokio::test]
    async fn nonexistent_dir() {
        let temp = TempDir::new().expect("temp dir");
        let tool = GlobTool;
        let args = serde_json::json!({ "pattern": "*.rs", "path": "nonexistent" });
        let ctx = make_ctx(temp.path());
        let result = tool.execute(args, &ctx).await.expect("execute");
        assert!(result.is_error);
    }
}
