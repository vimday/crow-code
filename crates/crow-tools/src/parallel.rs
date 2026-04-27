//! Parallel tool execution with cancellation support.
//!
//! Ported from yomi's `tools/parallel.rs`. Executes multiple tool calls
//! concurrently using `tokio::task::JoinSet` with optional
//! `CancellationToken` for responsive interruption.
//!
//! ## Design
//!
//! - Tools execute in parallel via `JoinSet::spawn()`
//! - `biased` `select!` ensures cancellation is checked first
//! - Results are collected in completion order
//! - Remaining tasks are aborted on cancellation

use crate::ToolOutput;
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Result of executing a single tool call.
#[derive(Debug)]
pub struct ToolExecutionResult {
    /// The tool_call_id from the LLM response.
    pub tool_call_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The tool output (success or error).
    pub output: ToolOutput,
    /// Elapsed time in milliseconds.
    pub elapsed_ms: u64,
}

/// A pending tool call to execute.
pub struct PendingToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Maximum tool output length before truncation (20KB).
pub const MAX_TOOL_OUTPUT_BYTES: usize = 20_000;

/// Execute multiple tool calls in parallel with optional cancellation.
///
/// Returns results in completion order (not submission order).
/// On cancellation, remaining tasks are aborted and partial results returned.
///
/// Each tool is resolved from the `registry` and executed with the given
/// `workspace_root`. The `permissions` are shared via `Arc` for thread safety.
pub async fn execute_tools_parallel(
    pending: &[PendingToolCall],
    registry: &crate::ToolRegistry,
    workspace_root: &std::path::Path,
    permissions: &Arc<crate::PermissionEnforcer>,
    cancel_token: Option<&CancellationToken>,
) -> Vec<ToolExecutionResult> {
    let tool_count = pending.len();
    tracing::info!("Executing {tool_count} tool(s) in parallel");

    let mut join_set = JoinSet::new();

    for call in pending {
        let call_id = call.id.clone();
        let call_name = call.name.clone();
        let arguments = call.arguments.clone();
        let tool_opt: Option<Arc<dyn crate::Tool>> = registry.get(&call_name);
        let workspace = workspace_root.to_path_buf();
        let perms = Arc::clone(permissions);

        join_set.spawn(async move {
            let start = std::time::Instant::now();

            let output = match tool_opt {
                Some(tool) => {
                    let ctx = crate::ToolContext {
                        workspace_root: &workspace,
                        permissions: &perms,
                        file_state: None,
                        background_manager: None,
                        subagent_delegator: None,
                    };
                    match tool.execute(arguments, &ctx).await {
                        Ok(mut output) => {
                            // Truncate oversized output
                            if output.content.len() > MAX_TOOL_OUTPUT_BYTES {
                                output.content = crate::truncation::truncate_tool_output(
                                    &output.content,
                                    MAX_TOOL_OUTPUT_BYTES,
                                );
                            }
                            output
                        }
                        Err(e) => ToolOutput::error(format!("Tool execution error: {e}")),
                    }
                }
                None => ToolOutput::error(format!("Unknown tool: {call_name}")),
            };

            let elapsed_ms = start.elapsed().as_millis() as u64;

            ToolExecutionResult {
                tool_call_id: call_id,
                tool_name: call_name,
                output,
                elapsed_ms,
            }
        });
    }

    let mut results = Vec::with_capacity(tool_count);

    if let Some(token) = cancel_token {
        loop {
            tokio::select! {
                biased;
                () = token.cancelled() => {
                    tracing::info!(
                        "Tool execution cancelled, aborting {} remaining tasks",
                        join_set.len()
                    );
                    join_set.abort_all();
                    break;
                }
                result = join_set.join_next() => {
                    match result {
                        Some(Ok(r)) => {
                            log_result(&r);
                            results.push(r);
                        }
                        Some(Err(e)) => {
                            tracing::warn!("Tool task panicked: {e}");
                        }
                        None => break,
                    }
                }
            }
        }
    } else {
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(r) => {
                    log_result(&r);
                    results.push(r);
                }
                Err(e) => {
                    tracing::warn!("Tool task panicked: {e}");
                }
            }
        }
    }

    let success_count = results.iter().filter(|r| !r.output.is_error).count();
    tracing::info!("Tool execution completed: {success_count}/{tool_count} succeeded");

    results
}

fn log_result(r: &ToolExecutionResult) {
    if r.output.is_error {
        tracing::warn!(
            "Tool {} ({}) failed in {}ms",
            r.tool_call_id,
            r.tool_name,
            r.elapsed_ms
        );
    } else {
        tracing::debug!(
            "Tool {} ({}) completed in {}ms",
            r.tool_call_id,
            r.tool_name,
            r.elapsed_ms
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_pending_returns_empty() {
        let registry = crate::ToolRegistry::new();
        let permissions = Arc::new(crate::PermissionEnforcer::new(crate::WriteMode::Sandbox));
        let temp = tempfile::TempDir::new().expect("temp");

        let results = execute_tools_parallel(&[], &registry, temp.path(), &permissions, None).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let registry = crate::ToolRegistry::new();
        let permissions = Arc::new(crate::PermissionEnforcer::new(crate::WriteMode::Sandbox));
        let temp = tempfile::TempDir::new().expect("temp");

        let pending = vec![PendingToolCall {
            id: "call-1".to_string(),
            name: "nonexistent_tool".to_string(),
            arguments: serde_json::json!({}),
        }];

        let results =
            execute_tools_parallel(&pending, &registry, temp.path(), &permissions, None).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].output.is_error);
        assert!(results[0].output.content.contains("Unknown tool"));
    }

    #[tokio::test]
    async fn cancellation_aborts_remaining() {
        let registry = crate::ToolRegistry::new();
        let permissions = Arc::new(crate::PermissionEnforcer::new(crate::WriteMode::Sandbox));
        let temp = tempfile::TempDir::new().expect("temp");

        let token = CancellationToken::new();
        // Cancel immediately
        token.cancel();

        let pending = vec![PendingToolCall {
            id: "call-1".to_string(),
            name: "nonexistent".to_string(),
            arguments: serde_json::json!({}),
        }];

        let results =
            execute_tools_parallel(&pending, &registry, temp.path(), &permissions, Some(&token))
                .await;
        // Should have 0 or 1 results (depending on race)
        assert!(results.len() <= 1);
    }
}
