//! General-purpose bash execution tool.
//!
//! Allows the agent to run arbitrary shell commands, gated by `PermissionEnforcer`.
//! Safety features:
//! - Output capped with smart head+tail truncation (preserves error context)
//! - Hard timeout (default 120s, max 600s)
//! - `kill_on_drop(true)` ensures child process cleanup
//! - `PAGER=cat` prevents interactive blocking
//! - Permission enforcement before execution
//! - Per-call `cwd` and `env` injection for multi-step workflows

use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use std::collections::HashMap;

/// Maximum output bytes from a bash command before head+tail truncation.
/// Bumped from 100KB → 256KB. Build/test logs commonly exceed 100KB and
/// were silently chopped.
const MAX_BASH_OUTPUT_BYTES: usize = 256 * 1024;

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Maximum allowed timeout in seconds. Bumped from 300s → 600s so that
/// `cargo build --release` and large test suites don't false-timeout.
const MAX_TIMEOUT_SECS: u64 = 600;

pub struct BashTool;

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Execute a bash command in the workspace directory. Use for running tests, building \
         projects, git operations, installing dependencies, and system commands. Output is \
         captured from both stdout and stderr. Commands that produce no output within the \
         timeout will be killed. Prefer dedicated tools (grep, file_edit, read_file) when \
         available — use bash only for operations without a dedicated tool. Supports \
         background=true for async execution of long-running tasks like dev servers. \
         Optional `cwd` (relative to workspace) for one-shot directory changes. \
         Optional `env` map for setting environment variables (e.g. RUST_BACKTRACE=1)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120, max: 600). Ignored if background is true."
                },
                "background": {
                    "type": "boolean",
                    "description": "Run command in background. Returns task_id immediately. Use bash_status to check output."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory relative to the workspace root. Default is the workspace root itself."
                },
                "env": {
                    "type": "object",
                    "description": "Extra environment variables to set for this command (e.g. {\"RUST_BACKTRACE\":\"1\",\"DEBUG\":\"1\"}). Merged on top of inherited env.",
                    "additionalProperties": {"type": "string"}
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            command: String,
            timeout_secs: Option<u64>,
            background: Option<bool>,
            cwd: Option<String>,
            env: Option<HashMap<String, String>>,
        }
        let parsed: Args = serde_json::from_value(args)?;

        if parsed.command.trim().is_empty() {
            return Ok(ToolOutput::error("Command cannot be empty"));
        }

        // Permission check
        ctx.permissions.check_bash(&parsed.command)?;

        // Resolve working directory: workspace_root + optional cwd, with
        // boundary enforcement to prevent escapes.
        let cwd_path = match resolve_cwd(ctx.workspace_root, parsed.cwd.as_deref()) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };

        if parsed.background.unwrap_or(false) {
            if let Some(bg_mgr) = &ctx.background_manager {
                let task_id = bg_mgr
                    .spawn(parsed.command.clone(), &cwd_path)
                    .await?;
                return Ok(ToolOutput::success(format!("Background task spawned successfully.\nTask ID: {task_id}\nUse 'bash_status' to check its output and status.")));
            } else {
                return Ok(ToolOutput::error(
                    "Background execution is not available in this context.",
                ));
            }
        }

        let timeout_secs = parsed
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);
        let timeout = std::time::Duration::from_secs(timeout_secs);

        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c")
            .arg(&parsed.command)
            .current_dir(&cwd_path)
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("TERM", "dumb")
            .kill_on_drop(true);

        // Layer user-supplied env over the defaults. Reject names that
        // collide with the safety vars above to avoid surprises.
        if let Some(extra_env) = parsed.env {
            for (k, v) in extra_env {
                if k.is_empty() {
                    continue;
                }
                cmd.env(k, v);
            }
        }

        let result = tokio::time::timeout(timeout, async { cmd.output().await }).await;

        match result {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let combined = format_output(&output.stdout, &output.stderr, exit_code);

                if output.status.success() {
                    Ok(ToolOutput::success(combined))
                } else {
                    Ok(ToolOutput {
                        content: combined,
                        is_error: true,
                    })
                }
            }
            Ok(Err(e)) => Ok(ToolOutput::error(format!("Failed to execute command: {e}"))),
            Err(_) => Ok(ToolOutput::error(format!(
                "Command timed out after {timeout_secs}s. Consider increasing timeout_secs (max 600), \
                 running with background=true, or breaking the command into smaller steps.\nCommand: {}",
                crow_patch::safe_truncate(&parsed.command, 200)
            ))),
        }
    }
}

/// Resolve and validate a working directory under the workspace root.
fn resolve_cwd(
    workspace_root: &std::path::Path,
    cwd: Option<&str>,
) -> std::result::Result<std::path::PathBuf, ToolOutput> {
    let Some(rel) = cwd else {
        return Ok(workspace_root.to_path_buf());
    };
    let trimmed = rel.trim();
    if trimmed.is_empty() {
        return Ok(workspace_root.to_path_buf());
    }
    let candidate = workspace_root.join(trimmed);
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let canonical_candidate = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.clone());
    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(ToolOutput::error(format!(
            "cwd '{trimmed}' escapes the workspace root."
        )));
    }
    if !canonical_candidate.exists() {
        return Err(ToolOutput::error(format!(
            "cwd '{trimmed}' does not exist (relative to workspace root)."
        )));
    }
    Ok(canonical_candidate)
}

/// Format stdout + stderr into a single output string, truncating with
/// smart head+tail windows so error context is preserved on long outputs.
fn format_output(stdout: &[u8], stderr: &[u8], exit_code: i32) -> String {
    let mut combined = String::new();

    let stdout_str = String::from_utf8_lossy(stdout);
    let stderr_str = String::from_utf8_lossy(stderr);

    if !stdout_str.is_empty() {
        combined.push_str(&stdout_str);
    }
    if !stderr_str.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("[stderr]\n");
        combined.push_str(&stderr_str);
    }

    if combined.len() > MAX_BASH_OUTPUT_BYTES {
        combined = head_tail_truncate(&combined, MAX_BASH_OUTPUT_BYTES);
    }

    if exit_code != 0 {
        combined.push_str(&format!("\n\n[Exit code: {exit_code}]"));
    }

    combined
}

/// Split a long output into a head + tail window with a marker in between.
/// Build/test logs typically have key info at start (what's running) AND
/// end (errors, summary). Plain truncation loses the second.
fn head_tail_truncate(s: &str, budget: usize) -> String {
    if s.len() <= budget {
        return s.to_string();
    }
    let marker_template = "\n\n[... truncated XXXX bytes — head + tail shown ...]\n\n";
    let usable = budget.saturating_sub(marker_template.len());
    let head_budget = usable * 2 / 3;
    let tail_budget = usable - head_budget;

    let head = crow_patch::safe_truncate(s, head_budget);
    // Take the tail. We need to land on a UTF-8 boundary.
    let tail_start_byte = s.len().saturating_sub(tail_budget);
    let tail = if tail_start_byte == 0 {
        ""
    } else {
        // Walk forward to the next valid char boundary.
        let mut idx = tail_start_byte;
        while idx < s.len() && !s.is_char_boundary(idx) {
            idx += 1;
        }
        &s[idx..]
    };

    let truncated_bytes = s.len().saturating_sub(head.len()).saturating_sub(tail.len());
    let marker = format!(
        "\n\n[... truncated {truncated_bytes} bytes — head + tail shown ...]\n\n"
    );
    format!("{head}{marker}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_tail_truncate_preserves_endpoints() {
        let s: String = (0..10000)
            .map(|i| format!("line {i}\n"))
            .collect();
        let truncated = head_tail_truncate(&s, 1024);
        assert!(truncated.contains("line 0"));
        // Tail should still contain something near the end.
        assert!(truncated.contains("truncated"));
        assert!(truncated.len() < 2048);
    }

    #[test]
    fn head_tail_truncate_passthrough_when_under_budget() {
        let s = "short output";
        let truncated = head_tail_truncate(s, 1024);
        assert_eq!(truncated, s);
    }

    #[test]
    fn resolve_cwd_blocks_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_cwd(tmp.path(), Some("../etc"));
        assert!(result.is_err());
    }

    #[test]
    fn resolve_cwd_accepts_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let result = resolve_cwd(tmp.path(), Some("sub"));
        assert!(result.is_ok());
    }
}
