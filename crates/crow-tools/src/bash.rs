//! General-purpose bash execution tool.
//!
//! Allows the agent to run arbitrary shell commands, gated by `PermissionEnforcer`.
//! Output is capped at 100KB. Commands have a 120s hard timeout.

use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;

/// Maximum output bytes from a bash command before truncation.
const MAX_BASH_OUTPUT_BYTES: usize = 100 * 1024;

pub struct BashTool;

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Execute a bash command in the workspace. Use this for running tests, installing dependencies, \
         building projects, git operations, and any other shell commands. The command runs in the \
         workspace root directory."
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
                    "description": "Optional timeout in seconds (default: 120, max: 300)"
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
        }
        let parsed: Args = serde_json::from_value(args)?;

        // Permission check
        ctx.permissions.check_bash(&parsed.command)?;

        let timeout = std::time::Duration::from_secs(
            parsed.timeout_secs.unwrap_or(120).min(300)
        );

        let result = tokio::time::timeout(timeout, async {
            tokio::process::Command::new("bash")
                .arg("-c")
                .arg(&parsed.command)
                .current_dir(ctx.frozen_root)
                .env("PAGER", "cat")       // Prevent paging in interactive commands
                .env("GIT_PAGER", "cat")   // Same for git
                .output()
                .await
        }).await;

        match result {
            Ok(Ok(output)) => {
                let mut combined = String::new();

                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if !stdout.is_empty() {
                    combined.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str("[stderr]\n");
                    combined.push_str(&stderr);
                }

                if combined.len() > MAX_BASH_OUTPUT_BYTES {
                    let truncated = crow_patch::safe_truncate(&combined, MAX_BASH_OUTPUT_BYTES);
                    combined = format!("{truncated}\n\n[SYSTEM WARNING: Output truncated to 100KB]");
                }

                let exit_code = output.status.code().unwrap_or(-1);
                if exit_code != 0 {
                    combined.push_str(&format!("\n\n[Exit code: {exit_code}]"));
                }

                if output.status.success() {
                    Ok(ToolOutput::success(combined))
                } else {
                    Ok(ToolOutput { content: combined, is_error: true })
                }
            }
            Ok(Err(e)) => Ok(ToolOutput::error(format!("Failed to execute command: {e}"))),
            Err(_) => Ok(ToolOutput::error(format!(
                "Command timed out after {} seconds: {}",
                timeout.as_secs(),
                parsed.command
            ))),
        }
    }
}
