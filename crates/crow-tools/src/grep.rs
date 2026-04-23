//! Ripgrep-powered search tool.
//!
//! Provides fast, intelligent code search with multiple output modes,
//! context lines, glob filtering, and pagination. Uses ripgrep (`rg`)
//! as the backend for maximum performance and .gitignore awareness.
//!
//! Inspired by Yomi's 700-line grep implementation.

use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;

const RIPGREP_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LIMIT: usize = 250;

pub struct GrepTool;

/// Configuration for a ripgrep invocation.
struct RgConfig<'a> {
    pattern: &'a str,
    output_mode: &'a str,
    context_before: usize,
    context_after: usize,
    show_line_numbers: bool,
    case_insensitive: bool,
    multiline: bool,
    glob_pattern: Option<&'a str>,
    file_type: Option<&'a str>,
}

impl GrepTool {
    /// Build ripgrep command arguments from parsed parameters.
    fn build_rg_args(cfg: &RgConfig<'_>) -> Vec<String> {
        let mut args = Vec::new();

        // Always include hidden files (match claude-code behavior)
        args.push("--hidden".into());

        // Line length limit to skip minified files
        args.push("--max-columns".into());
        args.push("500".into());

        // Multiline mode
        if cfg.multiline {
            args.push("-U".into());
            args.push("--multiline-dotall".into());
        }

        // Case insensitive
        if cfg.case_insensitive {
            args.push("-i".into());
        }

        // Output mode flags
        match cfg.output_mode {
            "files_with_matches" => args.push("-l".into()),
            "count" => args.push("-c".into()),
            _ => {
                // content mode
                if cfg.show_line_numbers {
                    args.push("-n".into());
                }
            }
        }

        // Context lines (only for content mode)
        if cfg.output_mode == "content" && (cfg.context_before > 0 || cfg.context_after > 0) {
            if cfg.context_before == cfg.context_after {
                args.push("-C".into());
                args.push(cfg.context_before.to_string());
            } else {
                if cfg.context_before > 0 {
                    args.push("-B".into());
                    args.push(cfg.context_before.to_string());
                }
                if cfg.context_after > 0 {
                    args.push("-A".into());
                    args.push(cfg.context_after.to_string());
                }
            }
        }

        // File type filter
        if let Some(ft) = cfg.file_type {
            args.push("--type".into());
            args.push(ft.to_string());
        }

        // Glob pattern filter
        if let Some(glob) = cfg.glob_pattern {
            for pat in glob.split_whitespace() {
                if !pat.is_empty() {
                    args.push("--glob".into());
                    args.push(pat.to_string());
                }
            }
        }

        // Exclude VCS directories
        args.push("--glob".into());
        args.push("!.git".into());
        args.push("--glob".into());
        args.push("!.svn".into());

        // Pattern — use -e to avoid interpretation as flag if starts with -
        if cfg.pattern.starts_with('-') {
            args.push("-e".into());
        }
        args.push(cfg.pattern.to_string());

        args
    }

    /// Apply offset and limit pagination to results.
    fn paginate(lines: &[&str], limit: usize, offset: usize) -> (String, bool) {
        let skip = offset.min(lines.len());
        let take = if limit == 0 {
            lines.len() - skip
        } else {
            (lines.len() - skip).min(limit)
        };
        let was_truncated = limit > 0 && (lines.len() - skip) > limit;
        let limited: Vec<&str> = lines.iter().skip(skip).take(take).copied().collect();

        let mut result = limited.join("\n");
        if was_truncated {
            result.push_str("\n\n(Results truncated. Use a more specific pattern, or increase limit.)");
        }
        (result, was_truncated)
    }
}

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents using regex patterns (powered by ripgrep). Supports multiple output \
         modes, context lines, glob filtering, and file type filtering. Respects .gitignore. \
         Always searches hidden files. Use this instead of bash grep/rg for structured results."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for in file contents"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in (relative to workspace). Defaults to workspace root."
                },
                "glob": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g., '*.rs', '*.{ts,tsx}')"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "Output mode: 'content' shows matching lines with context, 'files_with_matches' lists file paths only, 'count' shows match counts per file. Default: 'content'."
                },
                "context_before": {
                    "type": "integer",
                    "description": "Lines of context before each match (content mode only)"
                },
                "context_after": {
                    "type": "integer",
                    "description": "Lines of context after each match (content mode only)"
                },
                "context": {
                    "type": "integer",
                    "description": "Lines of context before and after each match (shorthand for context_before + context_after)"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Case insensitive search. Default: false."
                },
                "multiline": {
                    "type": "boolean",
                    "description": "Enable multiline mode where . matches newlines. Default: false."
                },
                "file_type": {
                    "type": "string",
                    "description": "Ripgrep file type filter (e.g., 'rust', 'py', 'js', 'go')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of output lines. Default: 250. Pass 0 for unlimited."
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip first N lines of output. Default: 0."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' argument"))?;

        if pattern.is_empty() {
            return Ok(ToolOutput::error("Search pattern cannot be empty"));
        }

        let path_str = args["path"].as_str();
        let glob_pattern = args["glob"].as_str();
        let file_type = args["file_type"].as_str();
        let output_mode = args["output_mode"].as_str().unwrap_or("content");
        let case_insensitive = args["case_insensitive"].as_bool().unwrap_or(false);
        let multiline = args["multiline"].as_bool().unwrap_or(false);
        let limit = args["limit"].as_u64().map(|n| n as usize).unwrap_or(DEFAULT_LIMIT);
        let offset = args["offset"].as_u64().map(|n| n as usize).unwrap_or(0);

        // Context lines
        let context = args["context"].as_u64().map(|n| n as usize);
        let context_before = context.unwrap_or_else(|| {
            args["context_before"].as_u64().map(|n| n as usize).unwrap_or(0)
        });
        let context_after = context.unwrap_or_else(|| {
            args["context_after"].as_u64().map(|n| n as usize).unwrap_or(0)
        });

        // Resolve search path
        let search_path: PathBuf = match path_str {
            Some(p) => ctx.frozen_root.join(p),
            None => ctx.frozen_root.to_path_buf(),
        };

        if !search_path.exists() {
            return Ok(ToolOutput::error(format!(
                "Path does not exist: {}",
                path_str.unwrap_or(".")
            )));
        }

        // Build ripgrep arguments
        let show_line_numbers = output_mode == "content";
        let rg_args = Self::build_rg_args(&RgConfig {
            pattern,
            output_mode,
            context_before,
            context_after,
            show_line_numbers,
            case_insensitive,
            multiline,
            glob_pattern,
            file_type,
        });

        // Run ripgrep
        let mut cmd = tokio::process::Command::new("rg");
        cmd.args(&rg_args)
            .arg(&search_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let result = tokio::time::timeout(RIPGREP_TIMEOUT, cmd.output()).await;

        let output = match result {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Ok(ToolOutput::error(format!(
                    "Failed to execute ripgrep: {e}. Is `rg` installed?"
                )));
            }
            Err(_) => {
                return Ok(ToolOutput::error(format!(
                    "Ripgrep timed out after {}s searching for '{pattern}'",
                    RIPGREP_TIMEOUT.as_secs()
                )));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        // Exit code 2 = ripgrep error
        if exit_code == 2 && !stderr.is_empty() {
            return Ok(ToolOutput::error(format!("ripgrep error: {stderr}")));
        }

        // Exit code 1 = no matches (not an error)
        if stdout.is_empty() {
            let msg = match output_mode {
                "files_with_matches" => "No files found matching the pattern",
                "count" => "No matches found",
                _ => "No matches found",
            };
            return Ok(ToolOutput::success(msg));
        }

        // Parse and paginate output
        let lines: Vec<&str> = stdout.lines().collect();
        let (result, _) = Self::paginate(&lines, limit, offset);

        // Add count summary for count mode
        let response = if output_mode == "count" {
            let total_matches: usize = lines.iter().filter_map(|line| {
                line.rsplit(':').next().and_then(|n| n.parse::<usize>().ok())
            }).sum();
            let file_count = lines.len();
            format!(
                "{result}\n\nFound {total_matches} total matches across {file_count} files"
            )
        } else {
            result
        };

        Ok(ToolOutput::success(response))
    }
}
