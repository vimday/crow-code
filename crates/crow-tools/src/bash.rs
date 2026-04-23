//! General-purpose bash execution tool.
//!
//! Allows the agent to run arbitrary shell commands, gated by `PermissionEnforcer`.
//! Safety features:
//! - Output capped at 100KB (UTF-8 safe truncation)
//! - Hard timeout (default 120s, max 300s)
//! - `kill_on_drop(true)` ensures child process cleanup
//! - `PAGER=cat` prevents interactive blocking
//! - Permission enforcement before execution

use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;

/// Maximum output bytes from a bash command before truncation.
const MAX_BASH_OUTPUT_BYTES: usize = 100 * 1024;

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Maximum allowed timeout in seconds.
const MAX_TIMEOUT_SECS: u64 = 300;

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
         available — use bash only for operations without a dedicated tool. Supports background=true \
         for async execution of long-running tasks like dev servers."
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
                    "description": "Timeout in seconds (default: 120, max: 300). Ignored if background is true."
                },
                "background": {
                    "type": "boolean",
                    "description": "Run command in background. Returns task_id immediately. Use bash_status to check output."
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
        }
        let parsed: Args = serde_json::from_value(args)?;

        if parsed.command.trim().is_empty() {
            return Ok(ToolOutput::error("Command cannot be empty"));
        }

        // Permission check
        ctx.permissions.check_bash(&parsed.command)?;

        if parsed.background.unwrap_or(false) {
            if let Some(bg_mgr) = &ctx.background_manager {
                let task_id = bg_mgr.spawn(parsed.command.clone(), ctx.workspace_root).await?;
                return Ok(ToolOutput::success(format!("Background task spawned successfully.\nTask ID: {task_id}\nUse 'bash_status' to check its output and status.")));
            } else {
                return Ok(ToolOutput::error("Background execution is not available in this context."));
            }
        }

        let timeout_secs = parsed.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).min(MAX_TIMEOUT_SECS);
        let timeout = std::time::Duration::from_secs(timeout_secs);

        let result = tokio::time::timeout(timeout, async {
            tokio::process::Command::new("bash")
                .arg("-c")
                .arg(&parsed.command)
                .current_dir(ctx.workspace_root)
                .env("PAGER", "cat")       // Prevent paging in interactive commands
                .env("GIT_PAGER", "cat")   // Same for git
                .env("TERM", "dumb")       // Disable color codes in some tools
                .kill_on_drop(true)        // Ensure child cleanup on cancellation
                .output()
                .await
        }).await;

        match result {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let combined = format_output(&output.stdout, &output.stderr, exit_code);

                if output.status.success() {
                    Ok(ToolOutput::success(combined))
                } else {
                    Ok(ToolOutput { content: combined, is_error: true })
                }
            }
            Ok(Err(e)) => Ok(ToolOutput::error(format!("Failed to execute command: {e}"))),
            Err(_) => Ok(ToolOutput::error(format!(
                "Command timed out after {timeout_secs}s. Consider increasing timeout_secs or \
                 breaking the command into smaller steps.\nCommand: {}",
                crow_patch::safe_truncate(&parsed.command, 200)
            ))),
        }
    }
}

/// Format stdout + stderr into a single output string, truncating if needed.
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

    // Truncate if output exceeds cap
    if combined.len() > MAX_BASH_OUTPUT_BYTES {
        let truncated = crow_patch::safe_truncate(&combined, MAX_BASH_OUTPUT_BYTES);
        combined = format!("{truncated}\n\n[Output truncated to 100KB]");
    }

    // Append exit code for failures
    if exit_code != 0 {
        combined.push_str(&format!("\n\n[Exit code: {exit_code}]"));
    }

    combined
}
