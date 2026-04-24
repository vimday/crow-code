//! Native tool-calling agent loop.
//!
//! Replaces the legacy `epistemic.rs` loop (custom AgentAction JSON parsing)
//! with a Codex-inspired streaming tool-call state machine. The agent:
//!
//! 1. Pre-sampling compaction: if context nears budget, compact before calling LLM
//! 2. Sends messages + tool definitions to the LLM provider via streaming
//! 3. Collects response: text chunks + tool_call requests
//! 4. If no tool_calls → conversation complete, return
//! 5. For each tool_call → execute via RwLock-gated parallel dispatch
//! 6. Append tool results as tool-role messages
//! 7. Mid-turn compaction: if context grew past budget from tool outputs, compact
//! 8. Loop back to step 1
//!
//! Key architectural features matching Codex parity:
//! - **Double-loop**: inner retry loop for transient LLM errors
//! - **Pre-sampling compaction**: compact before each LLM call (Codex `run_pre_sampling_compact`)
//! - **Mid-turn compaction**: compact after tool outputs grow context too large
//! - **RwLock parallelism**: read-only tools run in parallel, write tools acquire exclusive lock
//! - **CancellationToken propagation**: cancel reaches in-flight tool tasks via `tokio::select!`
//! - **Per-tool timeouts**: from `Tool::timeout()` instead of hardcoded 120s
//! - **Context-window-exceeded recovery**: auto-compact and retry on overflow errors

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::context::ConversationManager;
use crate::event::{AgentEvent, EventHandler};

// ─── Constants ──────────────────────────────────────────────────────

/// Maximum agent loop iterations before bailing out.
const MAX_AGENT_STEPS: usize = 40;

/// Maximum output bytes from a tool result before truncation.
const MAX_TOOL_OUTPUT_BYTES: usize = 100 * 1024; // 100 KB

/// Maximum number of tool calls to execute per response.
const MAX_TOOL_CALLS_PER_TURN: usize = 20;

/// Maximum retries for transient LLM errors (inner retry loop).
const MAX_LLM_RETRIES: u32 = 5;

// ─── Turn Configuration ─────────────────────────────────────────────

/// Aggregated configuration for a single agent turn.
///
/// Replaces the previous 9 bare function parameters, resolving
/// the `clippy::too_many_arguments` lint. Modeled after Codex's
/// `TurnContext` which bundles all per-turn state into one struct.
pub struct TurnConfig {
    pub compiler: Arc<crow_brain::IntentCompiler>,
    pub workspace_root: PathBuf,
    pub tool_registry: Arc<crow_tools::ToolRegistry>,
    pub permissions: Arc<crow_tools::PermissionEnforcer>,
    pub file_state: Arc<crow_tools::FileStateStore>,
    pub background_manager: Arc<crow_tools::BackgroundProcessManager>,
    pub subagent_delegator: Option<Arc<dyn crow_tools::SubagentDelegator>>,
    pub cancel_token: CancellationToken,
    /// Maximum agent loop steps before bailing out. Default: 40.
    pub max_steps: Option<usize>,
}

// ─── Turn Timing (Codex TurnTimingState pattern) ────────────────────

/// Timing data collected during a single agent turn.
#[derive(Debug, Clone)]
pub struct TurnTiming {
    /// Total wall-clock time for the entire agent turn.
    pub total_elapsed: std::time::Duration,
    /// Total time spent executing tool calls.
    pub tool_execution_time: std::time::Duration,
    /// Number of LLM API calls made during this turn (including retries).
    pub llm_call_count: u32,
    /// Number of pre-sampling compactions performed.
    pub compactions: u32,
}

impl Default for TurnTiming {
    fn default() -> Self {
        Self {
            total_elapsed: std::time::Duration::ZERO,
            tool_execution_time: std::time::Duration::ZERO,
            llm_call_count: 0,
            compactions: 0,
        }
    }
}

// ─── Agent Loop Result ──────────────────────────────────────────────

/// The outcome of a completed agent loop. Contains the final text response
/// and a record of all tool calls made.
#[derive(Debug, Clone)]
pub struct AgentLoopResult {
    /// The final text response from the agent (may be empty if the agent
    /// only communicated through tool calls).
    pub final_text: String,
    /// Total number of tool calls made during this turn.
    pub tool_call_count: usize,
    /// Turn timing data (Codex TurnTimingState pattern).
    pub timing: TurnTiming,
}

// ─── Agent Loop ─────────────────────────────────────────────────────

/// Run the native tool-calling agent loop until the LLM responds
/// without requesting any tool calls (i.e., it's done).
///
/// This is the replacement for `run_epistemic_loop`. Instead of parsing
/// custom `AgentAction` JSON, we use the provider's native tool calling
/// protocol. The loop drives:
///
/// ```text
/// [Pre-compact] → LLM Response → Parse tool_calls → Execute tools →
/// Feed results → [Mid-compact] → LLM Response → ...
/// ```
///
/// Returns when the LLM responds with text only (no tool calls).
pub async fn run_agent_loop(
    config: TurnConfig,
    messages: &mut ConversationManager,
    mut observer: &mut dyn EventHandler,
) -> Result<AgentLoopResult> {
    let turn_start = std::time::Instant::now();
    let mut timing = TurnTiming::default();
    let mut step = 0;
    let mut total_tool_calls = 0usize;
    let max_steps = config.max_steps.unwrap_or(MAX_AGENT_STEPS);

    // Get tool definitions from the registry (cached for the duration of the loop)
    let tool_defs = config.tool_registry.tool_definitions();

    // RwLock for read/write tool parallelism (Codex's `parallel_execution` pattern).
    // Read-only tools acquire a read lock (concurrent), write tools acquire a write lock (exclusive).
    let execution_lock: Arc<RwLock<()>> = Arc::new(RwLock::new(()));

    loop {
        step += 1;
        if step > max_steps {
            anyhow::bail!(
                "Agent loop exceeded {max_steps} steps without completing. Aborting."
            );
        }

        // ── Cancellation check ──────────────────────────────────────
        if config.cancel_token.is_cancelled() {
            observer.handle_event(AgentEvent::Log("Turn cancelled by user.".into()));
            timing.total_elapsed = turn_start.elapsed();
            return Ok(AgentLoopResult {
                final_text: String::new(),
                tool_call_count: total_tool_calls,
                timing,
            });
        }

        // ── Pre-sampling compaction (Codex pattern) ─────────────────
        // Check context budget BEFORE sending to the LLM. This prevents
        // context-window-exceeded errors from the provider.
        if messages.needs_compaction() {
            observer.handle_event(AgentEvent::Log(
                "    🔄 Pre-sampling compaction: context nearing limit...".into(),
            ));
            observer.handle_event(AgentEvent::Compacting { active: true });
            if let Err(e) = messages.compact_history(&config.compiler).await {
                observer.handle_event(AgentEvent::Log(format!(
                    "    ⚠️ Pre-sampling compaction failed: {e}"
                )));
            }
            timing.compactions += 1;
            observer.handle_event(AgentEvent::Compacting { active: false });
        }

        observer.handle_event(AgentEvent::StateChanged {
            from: "WaitingForInput".into(),
            to: "Streaming".into(),
        });
        observer.handle_event(AgentEvent::Thinking(step as u32, MAX_AGENT_STEPS as u32));

        // ── Stream LLM response with tools (inner retry loop) ───────
        let response = {
            struct ToolObserverAdapter<'a>(&'a mut dyn EventHandler);
            impl crow_brain::ToolStreamObserver for ToolObserverAdapter<'_> {
                fn on_text_chunk(&mut self, chunk: &str) {
                    self.0.handle_event(AgentEvent::StreamChunk(chunk.to_string()));
                }
                fn on_tool_call_start(&mut self, _id: &str, name: &str) {
                    self.0.handle_event(AgentEvent::ActionStart(
                        format!("Calling tool: {name}"),
                    ));
                }
                fn on_tool_call_args_chunk(&mut self, _id: &str, _chunk: &str) {
                    // Tool call argument streaming — handled internally by the client
                }
            }

            let mut adapter = ToolObserverAdapter(observer);
            let mut retry_count = 0u32;

            let result = loop {
                // Check cancellation before each LLM attempt
                if config.cancel_token.is_cancelled() {
                    break Err(crow_brain::BrainError::Config("Turn cancelled".into()));
                }

                match config.compiler.client()
                    .generate_streaming_with_tools(
                        &messages.as_messages(),
                        &tool_defs,
                        Some(&mut adapter),
                    )
                    .await
                {
                    Ok(resp) => break Ok(resp),
                    Err(ref brain_err) if is_context_overflow(brain_err) => {
                        // Context window exceeded — compact and retry once
                        adapter.0.handle_event(AgentEvent::Log(
                            "    🔄 Context window exceeded, compacting and retrying...".into(),
                        ));
                        adapter.0.handle_event(AgentEvent::Compacting { active: true });
                        let compact_result = messages.compact_history(&config.compiler).await;
                        adapter.0.handle_event(AgentEvent::Compacting { active: false });

                        if compact_result.is_err() || retry_count >= 1 {
                            break Err(crow_brain::BrainError::Config(
                                "Context window exceeded even after compaction".into(),
                            ));
                        }
                        retry_count += 1;
                        continue;
                    }
                    Err(ref brain_err) if brain_err.is_retryable() && retry_count < MAX_LLM_RETRIES => {
                        retry_count += 1;
                        let backoff_secs = 2u64.pow(retry_count);

                        // Suppress first retry event to reduce UI noise (Codex pattern)
                        if retry_count > 1 {
                            adapter.0.handle_event(AgentEvent::Retrying {
                                attempt: retry_count,
                                max_attempts: MAX_LLM_RETRIES,
                                reason: format!("Transient LLM error: {brain_err}"),
                            });
                        }

                        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    }
                    Err(e) => break Err(e),
                }
            };

            // Reclaim observer from adapter
            observer = adapter.0;
            result
        };

        let response = response.map_err(|e| anyhow::anyhow!("LLM call failed: {e:?}"))?;
        let response_text = response.text();
        let tool_calls = response.tool_calls();

        // ── No tool calls → agent is done ───────────────────────────
        if !response.has_tool_calls() {
            observer.handle_event(AgentEvent::StateChanged {
                from: "Streaming".into(),
                to: "Complete".into(),
            });

            // Stream the final text as markdown
            if !response_text.is_empty() {
                observer.handle_event(AgentEvent::Markdown(response_text.clone()));
            }

            // Record assistant response
            messages.push_assistant(&response_text);

            timing.total_elapsed = turn_start.elapsed();
            return Ok(AgentLoopResult {
                final_text: response_text,
                tool_call_count: total_tool_calls,
                timing,
            });
        }

        // ── Tool calls requested ────────────────────────────────────
        observer.handle_event(AgentEvent::StateChanged {
            from: "Streaming".into(),
            to: "ExecutingTool".into(),
        });

        // Stream any interleaved text before tool calls
        if !response_text.is_empty() {
            observer.handle_event(AgentEvent::Markdown(response_text.clone()));
        }

        // Record the assistant message with tool calls
        let tc_requests: Vec<crow_brain::ToolCallRequest> =
            tool_calls.iter().map(|tc| (*tc).clone()).collect();
        messages.push_assistant_with_tool_calls(&response_text, tc_requests);

        // Limit tool calls per response to prevent runaway
        let calls_to_execute = if tool_calls.len() > MAX_TOOL_CALLS_PER_TURN {
            observer.handle_event(AgentEvent::Log(format!(
                "    ⚠️ Tool call limit: executing first {MAX_TOOL_CALLS_PER_TURN} of {} calls",
                tool_calls.len()
            )));
            &tool_calls[..MAX_TOOL_CALLS_PER_TURN]
        } else {
            &tool_calls
        };

        // ── Execute tool calls with RwLock parallelism ──────────────
        let tool_exec_start = std::time::Instant::now();
        // Read-only tools acquire a shared read lock (concurrent).
        // Write tools acquire an exclusive write lock (serialized).
        // This matches Codex's `ToolCallRuntime` pattern from `parallel.rs`.
        let mut tasks = Vec::with_capacity(calls_to_execute.len());
        for tc in calls_to_execute {
            let registry = Arc::clone(&config.tool_registry);
            let tc_id = tc.id.clone();
            let tc_name = tc.name.clone();
            let tc_args = tc.arguments.clone();
            let root = config.workspace_root.clone();
            let perms = Arc::clone(&config.permissions);
            let fs = Arc::clone(&config.file_state);
            let bgm = Arc::clone(&config.background_manager);
            let delegator = config.subagent_delegator.clone();
            let lock = Arc::clone(&execution_lock);
            let tool_cancel = config.cancel_token.child_token();
            let tool_timeout = registry.tool_timeout(&tc_name);

            tasks.push(tokio::spawn(async move {
                let ctx = crow_tools::ToolContext {
                    workspace_root: &root,
                    permissions: &perms,
                    file_state: Some(fs),
                    background_manager: Some(bgm),
                    subagent_delegator: delegator,
                };

                // Determine lock type based on tool's read-only status
                let is_read_only = registry.is_read_only(&tc_name);

                // Execute with RwLock + cancellation + per-tool timeout
                let result = tokio::select! {
                    _ = tool_cancel.cancelled() => {
                        Err(anyhow::anyhow!("Tool '{tc_name}' aborted by user"))
                    }
                    result = async {
                        // Acquire appropriate lock
                        if is_read_only {
                            let _guard = lock.read().await;
                            tokio::time::timeout(
                                tool_timeout,
                                registry.execute(&tc_name, tc_args, &ctx),
                            ).await
                        } else {
                            let _guard = lock.write().await;
                            tokio::time::timeout(
                                tool_timeout,
                                registry.execute(&tc_name, tc_args, &ctx),
                            ).await
                        }
                    } => {
                        match result {
                            Ok(inner) => inner,
                            Err(_) => Err(anyhow::anyhow!(
                                "Tool '{tc_name}' timed out after {}s",
                                tool_timeout.as_secs()
                            )),
                        }
                    }
                };

                let output = match result {
                    Ok(out) => out,
                    Err(e) => crow_tools::ToolOutput::error(format!("Tool execution error: {e}")),
                };

                (tc_id, tc_name, output)
            }));
        }

        // Await all tool results
        let results = futures::future::join_all(tasks).await;

        for join_result in results {
            match join_result {
                Ok((tc_id, tc_name, output)) => {
                    total_tool_calls += 1;

                    let mut content = output.content;
                    if content.len() > MAX_TOOL_OUTPUT_BYTES {
                        // Safe truncation at a char boundary
                        let truncated =
                            crow_patch::safe_truncate(&content, MAX_TOOL_OUTPUT_BYTES);
                        content = format!(
                            "{truncated}\n\n[SYSTEM WARNING: Tool output truncated to 100KB]"
                        );
                    }

                    // Safe preview for the event (avoid UTF-8 boundary panics)
                    let preview = crow_patch::safe_truncate(&content, 120);
                    observer.handle_event(AgentEvent::ActionComplete(format!(
                        "{tc_name}: {preview}"
                    )));

                    if output.is_error {
                        observer.handle_event(AgentEvent::Log(format!(
                            "    ⚠️ Tool '{tc_name}' returned error"
                        )));
                    }

                    // Push tool result into conversation
                    messages.push_tool_result(&tc_id, &content);
                }
                Err(e) => {
                    observer.handle_event(AgentEvent::Error(format!(
                        "Tool execution panicked: {e}"
                    )));
                }
            }
        }

        timing.tool_execution_time += tool_exec_start.elapsed();

        // ── Mid-turn compaction (post-tool) ─────────────────────────
        // After tool results are added, check if context grew past budget.
        // This matches Codex's `run_auto_compact` mid-turn pattern.
        if messages.needs_compaction() {
            observer.handle_event(AgentEvent::Log(
                "    🔄 Mid-turn compaction: tool outputs grew context past budget...".into(),
            ));
            observer.handle_event(AgentEvent::Compacting { active: true });
            if let Err(e) = messages.compact_history(&config.compiler).await {
                observer.handle_event(AgentEvent::Log(format!(
                    "    ⚠️ Mid-turn compaction failed: {e}"
                )));
            }
            timing.compactions += 1;
            observer.handle_event(AgentEvent::Compacting { active: false });
        }
    }
}

/// Check if a brain error indicates the context window was exceeded.
fn is_context_overflow(err: &crow_brain::BrainError) -> bool {
    let msg = format!("{err:?}").to_lowercase();
    msg.contains("context_length_exceeded")
        || msg.contains("context window")
        || msg.contains("maximum context length")
        || msg.contains("token limit")
}
