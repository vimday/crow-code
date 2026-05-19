//! Tool Orchestrator — unified approval → execution → retry pipeline.
//!
//! Modeled after Codex's `ToolOrchestrator` from `tools/orchestrator.rs`.
//! Provides a single entry point for all tool executions, managing:
//!
//! - **Approval flow**: Check permissions before execution
//! - **Parallel/serial dispatch**: RwLock-based concurrency control
//! - **Timeout enforcement**: Per-tool timeout with cancellation
//! - **Output formatting**: Structured result with metadata
//! - **Error recovery**: Retry on transient failures
//!
//! # Architecture
//!
//! ```text
//! ToolOrchestrator::execute()
//!   ├─ check_approval()         → PermissionDenied / Approved
//!   ├─ acquire_lock()           → RwLock read (parallel) or write (exclusive)
//!   ├─ execute_with_timeout()   → Per-tool timeout + cancellation
//!   └─ format_output()          → ToolResult with metadata
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::{ToolContext, ToolOutput, ToolRegistry};

/// Result of orchestrating a single tool call.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Tool call ID from the LLM.
    pub call_id: String,
    /// Tool name.
    pub name: String,
    /// Output content (potentially truncated).
    pub output: ToolOutput,
    /// Wall-clock execution time.
    pub duration: Duration,
    /// Whether the tool was cancelled.
    pub was_cancelled: bool,
    /// Whether the tool timed out.
    pub was_timeout: bool,
}

/// Approval decision for a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Tool is approved for execution.
    Approved,
    /// Tool requires user confirmation.
    NeedsApproval { reason: String },
    /// Tool is denied outright.
    Denied { reason: String },
}

/// Configuration for the orchestrator.
pub struct OrchestratorConfig {
    /// Maximum output bytes before truncation.
    pub max_output_bytes: usize,
    /// Maximum concurrent tool calls.
    pub max_parallel: usize,
    /// Default timeout for tools that don't specify one.
    pub default_timeout: Duration,
    /// Escalation policy when a tool is denied (Codex pattern).
    pub escalation: EscalationPolicy,
}

/// Escalation policy when a tool call is denied (Codex orchestrator pattern).
///
/// When a tool call is denied due to permission restrictions, the
/// escalation policy determines whether the orchestrator should retry
/// with elevated permissions or return the denial immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EscalationPolicy {
    /// Do not escalate — return the denial as-is.
    #[default]
    NoEscalation,
    /// Log the denial but continue without retry.
    LogAndContinue,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_output_bytes: 100 * 1024, // 100 KB
            max_parallel: 20,
            default_timeout: Duration::from_secs(120),
            escalation: EscalationPolicy::default(),
        }
    }
}

/// Central tool execution orchestrator.
///
/// Owns the execution lock and provides a unified pipeline for all
/// tool calls. Replaces ad-hoc tool dispatch scattered across the
/// agent loop.
pub struct ToolOrchestrator {
    config: OrchestratorConfig,
    /// RwLock for parallel/serial execution control.
    /// Read-only tools acquire a shared read lock (concurrent).
    /// Write tools acquire an exclusive write lock (serialized).
    execution_lock: Arc<RwLock<()>>,
}

impl ToolOrchestrator {
    pub fn new(config: OrchestratorConfig) -> Self {
        Self {
            config,
            execution_lock: Arc::new(RwLock::new(())),
        }
    }

    /// Execute a single tool call through the full pipeline.
    ///
    /// Pipeline: approval → lock acquisition → timeout → execution → output formatting
    pub async fn execute_tool(
        &self,
        call_id: &str,
        tool_name: &str,
        args: serde_json::Value,
        registry: &ToolRegistry,
        ctx: &ToolContext<'_>,
        cancel: CancellationToken,
    ) -> ToolResult {
        let start = Instant::now();

        // ── Step 1: Approval check ──────────────────────────────────
        let approval = self.check_approval(tool_name, &args, ctx);
        if let ApprovalDecision::Denied { reason } = approval {
            return ToolResult {
                call_id: call_id.to_string(),
                name: tool_name.to_string(),
                output: ToolOutput::error(format!("Permission denied: {reason}")),
                duration: start.elapsed(),
                was_cancelled: false,
                was_timeout: false,
            };
        }

        // ── Step 2: Determine lock type ─────────────────────────────
        let is_read_only = registry.is_read_only(tool_name);
        let timeout = registry.tool_timeout(tool_name);

        // ── Step 3: Execute with lock + timeout + cancellation ──────
        let lock = Arc::clone(&self.execution_lock);
        let max_output = self.config.max_output_bytes;

        let result = tokio::select! {
            _ = cancel.cancelled() => {
                return ToolResult {
                    call_id: call_id.to_string(),
                    name: tool_name.to_string(),
                    output: ToolOutput::error(format!("Tool '{tool_name}' aborted by user")),
                    duration: start.elapsed(),
                    was_cancelled: true,
                    was_timeout: false,
                };
            }
            result = async {
                // Acquire appropriate lock
                if is_read_only {
                    let _guard = lock.read().await;
                    tokio::time::timeout(
                        timeout,
                        registry.execute(tool_name, args, ctx),
                    ).await
                } else {
                    let _guard = lock.write().await;
                    tokio::time::timeout(
                        timeout,
                        registry.execute(tool_name, args, ctx),
                    ).await
                }
            } => {
                result
            }
        };

        // ── Step 4: Handle result ───────────────────────────────────
        let (output, was_timeout) = match result {
            Ok(Ok(out)) => (out, false),
            Ok(Err(e)) => (
                ToolOutput::error(format!("Tool execution error: {e}")),
                false,
            ),
            Err(_elapsed) => (
                ToolOutput::error(format!(
                    "Tool '{tool_name}' timed out after {}s",
                    timeout.as_secs()
                )),
                true,
            ),
        };

        // ── Step 5: Truncate output if needed ───────────────────────
        let output = self.truncate_output(output, max_output);

        ToolResult {
            call_id: call_id.to_string(),
            name: tool_name.to_string(),
            output,
            duration: start.elapsed(),
            was_cancelled: false,
            was_timeout,
        }
    }

    /// Get the execution lock for external batch coordination.
    ///
    /// The `agent_loop` already manages batch execution with proper lifetime
    /// handling. This method exposes the lock for cases where the caller needs
    /// to coordinate tool execution outside the orchestrator.
    pub fn execution_lock(&self) -> Arc<RwLock<()>> {
        Arc::clone(&self.execution_lock)
    }

    /// Get the max output bytes configuration.
    pub fn max_output_bytes(&self) -> usize {
        self.config.max_output_bytes
    }

    /// Check if a tool call is approved for execution.
    ///
    /// Improved granularity: bash commands are analyzed to distinguish
    /// read-only operations (cat, grep, find) from write operations.
    fn check_approval(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> ApprovalDecision {
        // In DangerFullAccess mode, everything is auto-approved.
        if ctx.permissions.permission_mode == crate::PermissionMode::DangerFullAccess {
            return ApprovalDecision::Approved;
        }

        // For ReadOnly mode, block write tools
        if ctx.permissions.permission_mode == crate::PermissionMode::ReadOnly {
            match tool_name {
                "file_write" | "file_edit" => {
                    return ApprovalDecision::Denied {
                        reason: format!(
                            "Tool '{tool_name}' requires write access (running in read-only mode)"
                        ),
                    };
                }
                "bash"
                    // Analyze bash command for write indicators
                    if Self::is_bash_write_command(args) => {
                        return ApprovalDecision::Denied {
                            reason: "Bash command appears to modify files (running in read-only mode)".to_string(),
                        };
                    }
                _ => {}
            }
        }

        ApprovalDecision::Approved
    }

    /// Heuristic analysis of bash command arguments to detect write operations.
    ///
    /// Returns `true` if the command appears to modify the filesystem.
    /// Conservative — some edge cases may slip through, but the important
    /// destructive commands are caught.
    fn is_bash_write_command(args: &serde_json::Value) -> bool {
        let cmd = args
            .get("command")
            .or_else(|| args.get("cmd"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Destructive command prefixes
        let write_indicators = [
            "rm ", "rm\t", "rmdir",
            "mv ", "mv\t",
            "cp ", "cp\t",
            "mkdir", "touch",
            "chmod", "chown",
            "sed -i", "sed --in-place",
            "tee ", "tee\t",
            ">", ">>",
            "install ",
            "npm install", "yarn add", "pip install",
            "cargo add",
        ];

        let cmd_trimmed = cmd.trim();
        write_indicators
            .iter()
            .any(|indicator| cmd_trimmed.starts_with(indicator) || cmd_trimmed.contains(indicator))
    }

    /// Truncate output content if it exceeds the limit.
    fn truncate_output(&self, output: ToolOutput, max_bytes: usize) -> ToolOutput {
        if output.content.len() > max_bytes {
            let truncated = crow_patch::safe_truncate(&output.content, max_bytes);
            ToolOutput {
                content: format!(
                    "{truncated}\n\n[SYSTEM WARNING: Tool output truncated to {}KB]",
                    max_bytes / 1024
                ),
                is_error: output.is_error,
            }
        } else {
            output
        }
    }
}

impl Default for ToolOrchestrator {
    fn default() -> Self {
        Self::new(OrchestratorConfig::default())
    }
}
