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

use tokio::sync::{Mutex, RwLock};
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
    /// Number of retries that were performed (0 = first attempt succeeded).
    pub retry_count: u32,
    /// Whether this result came from the in-turn dedup cache (saved a real call).
    pub from_cache: bool,
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
    /// File ownership patterns from the agent role (Codex `AgentRole` pattern).
    /// When non-empty, write tools are restricted to files matching at least one pattern.
    /// Empty means all files are allowed.
    pub file_ownership: Vec<String>,
    /// Maximum retries for transient tool failures (network errors, timeouts).
    /// Default 2 (so up to 3 attempts total). Set to 0 to disable.
    pub max_tool_retries: u32,
    /// Enable in-turn dedup cache: identical (tool, args) calls within the
    /// same orchestrator return the previous output instead of re-executing.
    pub dedup_cache_enabled: bool,
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
            file_ownership: Vec::new(),
            max_tool_retries: 2,
            dedup_cache_enabled: true,
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
    /// In-turn dedup cache: keyed by (tool_name, canonical args JSON).
    /// Cleared when the orchestrator is dropped (one per turn).
    dedup_cache: Arc<Mutex<std::collections::HashMap<String, ToolOutput>>>,
}

impl ToolOrchestrator {
    pub fn new(config: OrchestratorConfig) -> Self {
        Self {
            config,
            execution_lock: Arc::new(RwLock::new(())),
            dedup_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Execute a single tool call through the full pipeline.
    ///
    /// Pipeline: dedup-cache → approval → lock acquisition → timeout → execution → retries → output formatting
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

        // ── Dedup cache lookup (read-only tools only — write tools always
        //    re-execute because side effects matter) ───────────────────
        let is_read_only = registry.is_read_only(tool_name);
        let cache_key = if self.config.dedup_cache_enabled && is_read_only {
            Some(make_dedup_key(tool_name, &args))
        } else {
            None
        };

        if let Some(ref key) = cache_key {
            let cache = self.dedup_cache.lock().await;
            if let Some(cached) = cache.get(key) {
                let cached_marker = ToolOutput {
                    content: format!(
                        "{}\n\n[CACHE: identical {tool_name} call already executed earlier this turn — reused result]",
                        cached.content
                    ),
                    is_error: cached.is_error,
                };
                return ToolResult {
                    call_id: call_id.to_string(),
                    name: tool_name.to_string(),
                    output: self.truncate_output(cached_marker, self.config.max_output_bytes),
                    duration: start.elapsed(),
                    was_cancelled: false,
                    was_timeout: false,
                    retry_count: 0,
                    from_cache: true,
                };
            }
        }

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
                retry_count: 0,
                from_cache: false,
            };
        }

        // ── Step 2: Determine lock type ─────────────────────────────
        let timeout = registry.tool_timeout(tool_name);

        // ── Step 3: Execute with retries ─────────────────────────────
        let max_retries = self.config.max_tool_retries;
        let lock = Arc::clone(&self.execution_lock);
        let max_output = self.config.max_output_bytes;
        let mut retry_count = 0u32;
        let mut final_output: ToolOutput;
        let was_timeout: bool;
        let mut was_cancelled = false;

        loop {
            let attempt_args = args.clone();
            let result = tokio::select! {
                _ = cancel.cancelled() => {
                    was_cancelled = true;
                    Ok(Err(anyhow::anyhow!("cancelled")))
                }
                result = async {
                    if is_read_only {
                        let _guard = lock.read().await;
                        tokio::time::timeout(
                            timeout,
                            registry.execute(tool_name, attempt_args, ctx),
                        ).await
                    } else {
                        let _guard = lock.write().await;
                        tokio::time::timeout(
                            timeout,
                            registry.execute(tool_name, attempt_args, ctx),
                        ).await
                    }
                } => {
                    result
                }
            };

            if was_cancelled {
                return ToolResult {
                    call_id: call_id.to_string(),
                    name: tool_name.to_string(),
                    output: ToolOutput::error(format!("Tool '{tool_name}' aborted by user")),
                    duration: start.elapsed(),
                    was_cancelled: true,
                    was_timeout: false,
                    retry_count,
                    from_cache: false,
                };
            }

            let (output, this_was_timeout) = match result {
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

            let should_retry = retry_count < max_retries
                && (this_was_timeout || is_transient_failure(&output));

            if should_retry {
                retry_count += 1;
                // Exponential backoff: 200ms → 400ms → 800ms → 1.6s
                let backoff_ms = 200u64 << retry_count.saturating_sub(1).min(4);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                continue;
            }

            final_output = output;
            was_timeout = this_was_timeout;
            break;
        }

        // ── Step 4: Truncate output if needed ───────────────────────
        final_output = self.truncate_output(final_output, max_output);

        // ── Step 5: Cache successful read-only results for dedup ────
        if let Some(key) = cache_key {
            if !final_output.is_error {
                let mut cache = self.dedup_cache.lock().await;
                cache.insert(key, final_output.clone());
            }
        }

        ToolResult {
            call_id: call_id.to_string(),
            name: tool_name.to_string(),
            output: final_output,
            duration: start.elapsed(),
            was_cancelled: false,
            was_timeout,
            retry_count,
            from_cache: false,
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

        // File ownership check (Codex AgentRole pattern)
        // If file_ownership patterns are configured, deny writes to files
        // not matching any pattern.
        if !self.config.file_ownership.is_empty() {
            let is_write_tool = matches!(tool_name, "file_write" | "file_edit" | "bash");
            if is_write_tool {
                if let Some(path) = args.get("path").or_else(|| args.get("file")).and_then(|v| v.as_str()) {
                    let owned = self.config.file_ownership.iter().any(|pattern| {
                        path.contains(pattern) || glob_matches(pattern, path)
                    });
                    if !owned {
                        return ApprovalDecision::Denied {
                            reason: format!(
                                "File '{path}' is outside this role's ownership scope"
                            ),
                        };
                    }
                }
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

/// Simple glob pattern matching for file ownership checks.
/// Supports `*` as wildcard and `**/` for recursive directory matching.
fn glob_matches(pattern: &str, path: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("**/") {
        return path.ends_with(suffix) || path.contains(&format!("/{suffix}"));
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return path.starts_with(prefix);
    }
    path.contains(pattern)
}

/// Build a stable dedup key from tool name + canonical-form args JSON.
/// We sort object keys recursively so semantically-equivalent calls hash
/// identically even if the LLM reordered fields.
fn make_dedup_key(tool_name: &str, args: &serde_json::Value) -> String {
    let canonical = canonicalize_json(args);
    format!("{tool_name}|{canonical}")
}

fn canonicalize_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut s = String::from("{");
            for (i, (k, val)) in entries.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push('"');
                s.push_str(k);
                s.push_str("\":");
                s.push_str(&canonicalize_json(val));
            }
            s.push('}');
            s
        }
        serde_json::Value::Array(arr) => {
            let mut s = String::from("[");
            for (i, val) in arr.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&canonicalize_json(val));
            }
            s.push(']');
            s
        }
        other => other.to_string(),
    }
}

/// Heuristic: does this tool output look like a transient failure
/// worth retrying? Catches common network/timeout error strings without
/// punishing legitimate errors that won't recover by waiting.
fn is_transient_failure(output: &ToolOutput) -> bool {
    if !output.is_error {
        return false;
    }
    let text = output.content.to_lowercase();
    let transient_markers = [
        "timed out",
        "timeout",
        "connection refused",
        "connection reset",
        "broken pipe",
        "temporarily unavailable",
        "503",
        "502",
        "504",
        "rate limit",
        "too many requests",
        "429",
        "no route to host",
        "network unreachable",
    ];
    transient_markers.iter().any(|m| text.contains(m))
}

#[cfg(test)]
mod orchestrator_helpers_tests {
    use super::*;

    #[test]
    fn dedup_key_is_order_independent() {
        let a = serde_json::json!({"path": "src/foo.rs", "limit": 100});
        let b = serde_json::json!({"limit": 100, "path": "src/foo.rs"});
        assert_eq!(make_dedup_key("read_file", &a), make_dedup_key("read_file", &b));
    }

    #[test]
    fn dedup_key_distinguishes_different_args() {
        let a = serde_json::json!({"path": "src/foo.rs"});
        let b = serde_json::json!({"path": "src/bar.rs"});
        assert_ne!(make_dedup_key("read_file", &a), make_dedup_key("read_file", &b));
    }

    #[test]
    fn detects_transient_errors() {
        let timeout = ToolOutput::error("Tool 'bash' timed out after 120s");
        let rate_limit = ToolOutput::error("HTTP 429 rate limit exceeded");
        let success = ToolOutput::success("done");
        let perm = ToolOutput::error("Permission denied: file is read-only");
        assert!(is_transient_failure(&timeout));
        assert!(is_transient_failure(&rate_limit));
        assert!(!is_transient_failure(&success));
        assert!(!is_transient_failure(&perm));
    }
}
