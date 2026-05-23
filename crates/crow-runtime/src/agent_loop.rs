//! Native tool-calling agent loop.
//!
//! Replaces the legacy `epistemic.rs` loop (custom AgentAction JSON parsing)
//! with a Codex-inspired streaming tool-call state machine. The agent:
//!
//! 1. Pre-sampling compaction: if context nears budget, compact before calling LLM
//! 2. Sends messages + tool definitions to the LLM provider via streaming
//! 3. Collects response: text chunks + tool_call requests
//! 4. If no tool_calls → conversation complete, return
//! 5. For each tool_call → execute via `ToolOrchestrator` pipeline
//! 6. Append tool results as tool-role messages
//! 7. Mid-turn compaction: if context grew past budget from tool outputs, compact
//! 8. Loop back to step 1
//!
//! Key architectural features matching Codex parity:
//! - **TurnContext**: immutable per-turn snapshot replaces scattered config
//! - **ToolOrchestrator**: unified approval → lock → timeout → truncate pipeline
//! - **Double-loop**: inner retry loop for transient LLM errors
//! - **Pre-sampling compaction**: compact before each LLM call (Codex `run_pre_sampling_compact`)
//! - **Mid-turn compaction**: compact after tool outputs grow context too large
//! - **RwLock parallelism**: via ToolOrchestrator (read-only = shared, write = exclusive)
//! - **CancellationToken propagation**: cancel reaches in-flight tool tasks via `tokio::select!`
//! - **Per-tool timeouts**: from `Tool::timeout()` via ToolOrchestrator
//! - **Context-window-exceeded recovery**: auto-compact and retry on overflow errors
//! - **Role constraint enforcement**: max_steps, max_tool_calls, max_turn_duration from AgentRole
//! - **AgentStatusTracker**: observable state machine updated at turn lifecycle boundaries
//! - **TurnTimingState**: async-safe TTFT/TTFM/duration tracking via the canonical module

use anyhow::Result;
use std::sync::Arc;

use crate::agent_status::AgentStatus;
use crate::context::ConversationManager;
use crate::event::{AgentEvent, EventHandler, TurnEvent, TurnPhase};
use crate::turn_context::TurnContext;

// ─── Constants ──────────────────────────────────────────────────────

/// Maximum output bytes from a tool result before truncation.
const MAX_TOOL_OUTPUT_BYTES: usize = 100 * 1024; // 100 KB

/// Fallback maximum tool calls per response (overridden by role).
const DEFAULT_MAX_TOOL_CALLS_PER_TURN: usize = 20;

/// Maximum retries for transient LLM errors (inner retry loop).
const MAX_LLM_RETRIES: u32 = 5;

/// Default turn-level timeout when the role doesn't specify one (10 minutes).
const DEFAULT_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

// ─── Turn Metrics ───────────────────────────────────────────────────

/// Supplementary turn metrics tracked inline during the agent loop.
///
/// The canonical async-safe timing (TTFT/TTFM/duration) lives in
/// `TurnTimingState` (`turn_timing.rs`) and is accessed via `ctx.timing`.
/// This struct captures additional counters that `TurnTimingState` doesn't
/// track (tool execution time, compaction count, LLM call count).
#[derive(Debug, Clone, Default)]
pub struct TurnMetrics {
    /// Total time spent executing tool calls.
    pub tool_execution_time: std::time::Duration,
    /// Number of LLM API calls made during this turn (including retries).
    pub llm_call_count: u32,
    /// Number of pre-sampling compactions performed.
    pub compactions: u32,
}

impl TurnMetrics {
    /// Return a human-readable summary combining these metrics with a timing snapshot.
    pub fn summary(&self, snapshot: &crate::turn_timing::TurnTimingSnapshot) -> String {
        let total_ms = snapshot.total_ms;
        let tool_ms = self.tool_execution_time.as_millis() as u64;
        let llm_ms = total_ms.saturating_sub(tool_ms);
        let ttft = snapshot
            .ttft_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "n/a".to_string());
        format!(
            "Turn: {total_ms}ms total, {llm_ms}ms LLM ({} calls), {tool_ms}ms tools, TTFT: {ttft}, {} compaction(s)",
            self.llm_call_count, self.compactions
        )
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
    /// Canonical timing snapshot (TTFT, TTFM, total duration).
    pub timing_snapshot: Option<crate::turn_timing::TurnTimingSnapshot>,
    /// Supplementary turn metrics (tool time, LLM calls, compactions).
    pub metrics: TurnMetrics,
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
    ctx: &TurnContext,
    messages: &mut ConversationManager,
    observer: &mut dyn EventHandler,
) -> Result<AgentLoopResult> {
    // ── Role-aware constraints ───────────────────────────────────
    // Merge role limits with TurnContext defaults. The tighter bound wins.
    let effective_max_steps = ctx.max_steps.min(ctx.role.max_steps);
    let effective_max_tool_calls = ctx.role.max_tool_calls_per_turn.min(DEFAULT_MAX_TOOL_CALLS_PER_TURN);
    let turn_timeout = if ctx.role.max_turn_duration.as_secs() > 0 {
        ctx.role.max_turn_duration
    } else {
        DEFAULT_TURN_TIMEOUT
    };

    // Wrap the entire loop body in a turn-level timeout (Codex pattern).
    // Subagents already have a 120s hard timeout; this provides the same
    // safety net for the main agent loop.
    match tokio::time::timeout(
        turn_timeout,
        run_agent_loop_inner(ctx, messages, observer, effective_max_steps, effective_max_tool_calls),
    )
    .await
    {
        Ok(result) => result,
        Err(_elapsed) => {
            // Turn-level timeout exceeded
            observer.handle_event(AgentEvent::Turn(TurnEvent::Aborted {
                turn_id: ctx.turn_id.clone(),
                reason: format!("Turn exceeded {}s timeout", turn_timeout.as_secs()),
            }));
            ctx.status_tracker.set(AgentStatus::Errored(
                format!("Turn timeout after {}s", turn_timeout.as_secs()),
            ));
            messages.sanitize();
            let snapshot = ctx.timing.snapshot().await;
            Ok(AgentLoopResult {
                final_text: String::new(),
                tool_call_count: 0,
                timing_snapshot: snapshot,
                metrics: TurnMetrics::default(),
            })
        }
    }
}

/// Inner agent loop body — extracted so we can wrap it in `tokio::time::timeout`.
async fn run_agent_loop_inner(
    ctx: &TurnContext,
    messages: &mut ConversationManager,
    mut observer: &mut dyn EventHandler,
    effective_max_steps: usize,
    effective_max_tool_calls: usize,
) -> Result<AgentLoopResult> {
    let mut metrics = TurnMetrics::default();
    let mut step = 0;
    let mut total_tool_calls = 0usize;

    // ── Wire canonical TurnTimingState (replaces inline TurnTiming) ──
    ctx.timing.mark_turn_started().await;

    // ── Wire AgentStatusTracker (Codex observable state machine) ─────
    ctx.status_tracker.set(AgentStatus::Running);

    // Turn-level diff tracker lives in TurnContext (Codex pattern).
    // Reset it for this turn in case TurnContext is reused.
    ctx.diff_tracker.lock().await.reset();

    // Get tool definitions from the registry (cached for the duration of the loop)
    let tool_defs = ctx.tool_registry.tool_definitions();

    // ── Emit TurnEvent::Started (Codex turn lifecycle) ───────────
    observer.handle_event(AgentEvent::Turn(TurnEvent::Started {
        turn_id: ctx.turn_id.clone(),
    }));

    // ── Inject environment context at turn start (Codex pattern) ─
    // Give the agent situational awareness about its runtime
    // environment: OS, shell, date, cwd.
    let env_ctx = crow_brain::environment::CrowEnvironmentContext::from_workspace(
        &ctx.workspace_root,
    );
    let env_block = env_ctx.render();
    if !env_block.is_empty() {
        // Inject as a synthetic user message so the agent sees it
        // in its first sampling call.
        messages.push_user(format!(
            "[SYSTEM: Environment context for this turn]\n{env_block}"
        ));
    }

    // Create a ToolOrchestrator for this turn (owns the RwLock for parallel/serial dispatch)
    let orchestrator = Arc::new(crow_tools::orchestrator::ToolOrchestrator::new(
        crow_tools::orchestrator::OrchestratorConfig {
            max_output_bytes: MAX_TOOL_OUTPUT_BYTES,
            max_parallel: effective_max_tool_calls,
            file_ownership: ctx.role.file_ownership.clone(),
            ..Default::default()
        },
    ));

    loop {
        step += 1;
        if step > effective_max_steps {
            ctx.status_tracker.set(AgentStatus::Errored(
                format!("Exceeded {effective_max_steps} steps"),
            ));
            anyhow::bail!(
                "Agent loop exceeded {effective_max_steps} steps without completing. Aborting."
            );
        }

        // ── Proactive sanitization (Codex normalize.rs pattern) ────
        // Before each sampling call, sanitize the conversation buffer:
        //   1. Synthesize missing tool outputs for interrupted calls
        //   2. Remove orphan tool results
        //   3. Ensure first message is User
        //   4. Fix strict role alternation
        // This prevents API 400 errors from conversation drift.
        if step > 1 {
            messages.sanitize();
        }

        // ── Cancellation check ──────────────────────────────────────
        if ctx.is_cancelled() {
            // Post-cancellation sanitization (Codex pattern):
            // Ensure conversation state is valid before returning.
            messages.sanitize();
            observer.handle_event(AgentEvent::Log("Turn cancelled by user.".into()));
            observer.handle_event(AgentEvent::Turn(
                crate::event::TurnEvent::Aborted {
                    turn_id: ctx.turn_id.clone(),
                    reason: "Cancelled by user".into(),
                },
            ));
            ctx.status_tracker.set(AgentStatus::Interrupted);
            let snapshot = ctx.timing.snapshot().await;
            return Ok(AgentLoopResult {
                final_text: String::new(),
                tool_call_count: total_tool_calls,
                timing_snapshot: snapshot,
                metrics,
            });
        }

        // ── Pre-sampling compaction (Codex pattern) ─────────────────
        // Check context budget BEFORE sending to the LLM. This prevents
        // context-window-exceeded errors from the provider.
        if messages.needs_compaction() {
            observer.handle_event(AgentEvent::Turn(TurnEvent::PhaseChanged {
                turn_id: ctx.turn_id.clone(),
                phase: TurnPhase::Compacting,
            }));
            observer.handle_event(AgentEvent::Log(
                "    🔄 Pre-sampling compaction: context nearing limit...".into(),
            ));
            observer.handle_event(AgentEvent::Compacting { active: true });
            if let Err(e) = messages.compact_history(&ctx.compiler).await {
                observer.handle_event(AgentEvent::Log(format!(
                    "    ⚠️ Pre-sampling compaction failed: {e}"
                )));
            }
            metrics.compactions += 1;
            observer.handle_event(AgentEvent::Compacting { active: false });
            // Codex pattern: warn user about accuracy degradation after compaction
            observer.handle_event(AgentEvent::Log(
                "    ⚠️ Long threads and compactions can reduce accuracy. Start a new session when possible.".into(),
            ));
        }

        observer.handle_event(AgentEvent::Turn(TurnEvent::PhaseChanged {
            turn_id: ctx.turn_id.clone(),
            phase: TurnPhase::EpistemicLoop {
                step: step as u32,
                max_steps: effective_max_steps as u32,
            },
        }));
        observer.handle_event(AgentEvent::StateChanged {
            from: "WaitingForInput".into(),
            to: "Streaming".into(),
        });
        observer.handle_event(AgentEvent::Thinking(step as u32, effective_max_steps as u32));

        // ── Stream LLM response with tools (inner retry loop) ───────
        let response = {
            struct ToolObserverAdapter<'a>(&'a mut dyn EventHandler);
            impl crow_brain::ToolStreamObserver for ToolObserverAdapter<'_> {
                fn on_text_chunk(&mut self, chunk: &str) {
                    self.0
                        .handle_event(AgentEvent::StreamChunk(chunk.to_string()));
                }
                fn on_tool_call_start(&mut self, _id: &str, name: &str) {
                    self.0
                        .handle_event(AgentEvent::ActionStart(format!("Calling tool: {name}")));
                }
                fn on_tool_call_args_chunk(&mut self, _id: &str, _chunk: &str) {
                    // Tool call argument streaming — handled internally by the client
                }
            }

            let mut adapter = ToolObserverAdapter(observer);
            let mut retry_count = 0u32;
            let _llm_call_start = std::time::Instant::now();

            let result = loop {
                // Check cancellation before each LLM attempt
                if ctx.is_cancelled() {
                    break Err(crow_brain::BrainError::Config("Turn cancelled".into()));
                }

                match ctx
                    .compiler
                    .client()
                    .generate_streaming_with_tools(
                        &messages.as_messages(),
                        &tool_defs,
                        Some(&mut adapter),
                    )
                    .await
                {
                    Ok(resp) => {
                        metrics.llm_call_count += 1;
                        // Record TTFT via canonical TurnTimingState
                        ctx.timing.record_first_token().await;
                        break Ok(resp);
                    }
                    Err(ref brain_err) if is_context_overflow(brain_err) => {
                        // Context window exceeded — compact and retry once
                        adapter.0.handle_event(AgentEvent::Log(
                            "    🔄 Context window exceeded, compacting and retrying...".into(),
                        ));
                        adapter
                            .0
                            .handle_event(AgentEvent::Compacting { active: true });
                        let compact_result = messages.compact_history(&ctx.compiler).await;
                        adapter
                            .0
                            .handle_event(AgentEvent::Compacting { active: false });

                        if compact_result.is_err() || retry_count >= 1 {
                            break Err(crow_brain::BrainError::Config(
                                "Context window exceeded even after compaction".into(),
                            ));
                        }
                        retry_count += 1;
                        continue;
                    }
                    Err(ref brain_err)
                        if brain_err.is_retryable() && retry_count < MAX_LLM_RETRIES =>
                    {
                        retry_count += 1;

                        // Suppress first retry event to reduce UI noise (Codex pattern)
                        if retry_count > 1 {
                            adapter.0.handle_event(AgentEvent::Retrying {
                                attempt: retry_count,
                                max_attempts: MAX_LLM_RETRIES,
                                reason: format!("Transient LLM error: {brain_err}"),
                            });
                        }

                        tokio::time::sleep(crate::turn_timing::backoff_with_jitter(retry_count)).await;
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

            // ── Emit turn diff via TurnEvent::DiffGenerated ─────────
            // Compute aggregated unified diff for all files modified
            // during this turn and emit it through the event system
            // so the TUI /diff command can display it.
            let diff_guard = ctx.diff_tracker.lock().await;
            if let Some(diff_text) = diff_guard.unified_diff() {
                let change_summary = diff_guard.change_summary();
                if !change_summary.is_empty() {
                    let summary_lines: Vec<String> = change_summary
                        .iter()
                        .map(|(p, k)| format!("  {k}: {}", p.display()))
                        .collect();
                    observer.handle_event(AgentEvent::Log(format!(
                        "    📝 Turn modified {} file(s):\n{}",
                        change_summary.len(),
                        summary_lines.join("\n")
                    )));
                }
                // Emit structured diff event (replaces the old `let _ = diff;` discard)
                observer.handle_event(AgentEvent::Turn(
                    crate::event::TurnEvent::DiffGenerated {
                        turn_id: ctx.turn_id.clone(),
                        diff_text,
                        files_changed: change_summary.len(),
                    },
                ));
            }
            drop(diff_guard);

            // Record first complete message via canonical TurnTimingState
            ctx.timing.record_first_message().await;
            let snapshot = ctx.timing.snapshot().await;

            // ── Emit TurnEvent::Completed (Codex turn lifecycle) ─────
            observer.handle_event(AgentEvent::Turn(TurnEvent::Completed {
                turn_id: ctx.turn_id.clone(),
                success: true,
                token_usage: None,
            }));
            ctx.status_tracker.set(AgentStatus::Completed(None));

            return Ok(AgentLoopResult {
                final_text: response_text,
                tool_call_count: total_tool_calls,
                timing_snapshot: snapshot,
                metrics,
            });
        }

        // ── Tool calls requested ────────────────────────────────────
        observer.handle_event(AgentEvent::Turn(TurnEvent::PhaseChanged {
            turn_id: ctx.turn_id.clone(),
            phase: TurnPhase::Applying,
        }));
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

        // ── TurnDiffTracker: snapshot files targeted by write tools ──
        // Before executing tool calls, snapshot any files that write-tools
        // (file_edit, file_write, bash) might modify. This captures the
        // baseline so we can produce accurate diffs at turn end.
        {
            let mut diff_guard = ctx.diff_tracker.lock().await;
            for tc in &tool_calls {
                if !ctx.tool_registry.is_read_only(&tc.name) {
                    // Try to extract file path from tool arguments
                    if let Some(path) = extract_target_path(&tc.arguments, &ctx.workspace_root) {
                        diff_guard.snapshot_before_modify(&path);
                    }
                }
            }
        }

        // Limit tool calls per response to prevent runaway (role-aware)
        let calls_to_execute = if tool_calls.len() > effective_max_tool_calls {
            observer.handle_event(AgentEvent::Log(format!(
                "    ⚠️ Tool call limit: executing first {effective_max_tool_calls} of {} calls",
                tool_calls.len()
            )));
            &tool_calls[..effective_max_tool_calls]
        } else {
            &tool_calls
        };

        // ── Execute tool calls via ToolOrchestrator ─────────────────
        // Each tool call is dispatched through the orchestrator's unified
        // pipeline (approval → lock → timeout → truncation → cancellation).
        let tool_exec_start = std::time::Instant::now();
        let mut tasks = Vec::with_capacity(calls_to_execute.len());
        for tc in calls_to_execute {
            let tc_id = tc.id.clone();
            let tc_name = tc.name.clone();
            let tc_args = tc.arguments.clone();
            let root = ctx.workspace_root.clone();
            let perms = Arc::clone(&ctx.permissions);
            let fs = Arc::clone(&ctx.file_state);
            let bgm = Arc::clone(&ctx.background_manager);
            let delegator = ctx.subagent_delegator.clone();
            let registry = Arc::clone(&ctx.tool_registry);
            let cancel_token = ctx.child_cancel_token();
            let orch = Arc::clone(&orchestrator);

            // Emit structured ToolCallStarted event (Codex pattern)
            let is_read_only = ctx.tool_registry.is_read_only(&tc_name);
            observer.handle_event(AgentEvent::ToolCallStarted {
                call_id: tc_id.clone(),
                tool_name: tc_name.clone(),
                is_read_only,
            });

            tasks.push(tokio::spawn(async move {
                let tool_ctx = crow_tools::ToolContext {
                    workspace_root: &root,
                    permissions: &perms,
                    file_state: Some(fs),
                    background_manager: Some(bgm),
                    subagent_delegator: delegator,
                };

                // Dispatch through the orchestrator's unified pipeline
                let result = orch.execute_tool(
                    &tc_id,
                    &tc_name,
                    tc_args,
                    &registry,
                    &tool_ctx,
                    cancel_token,
                ).await;

                result
            }));
        }

        // Await all tool results
        let results = futures::future::join_all(tasks).await;

        for join_result in results {
            match join_result {
                Ok(tool_result) => {
                    total_tool_calls += 1;

                    let content = tool_result.output.content.clone();
                    let is_error = tool_result.output.is_error;

                    // Emit structured ToolCallCompleted event (Codex pattern)
                    observer.handle_event(AgentEvent::ToolCallCompleted {
                        call_id: tool_result.call_id.clone(),
                        tool_name: tool_result.name.clone(),
                        duration_ms: tool_result.duration.as_millis() as u64,
                        output_bytes: content.len(),
                        is_error,
                    });

                    // Safe preview for the legacy ActionComplete event
                    let preview = crow_patch::safe_truncate(&content, 120);
                    observer.handle_event(AgentEvent::ActionComplete(
                        format!("{}: {preview}", tool_result.name),
                    ));

                    if is_error {
                        observer.handle_event(AgentEvent::Log(format!(
                            "    ⚠️ Tool '{}' returned error",
                            tool_result.name
                        )));
                    }

                    // Push tool result into conversation
                    messages.push_tool_result(&tool_result.call_id, &content);
                }
                Err(e) => {
                    // Synthesize a tool result for the panicked call to prevent
                    // orphan tool_call → API 400 errors (Codex ensure_tool_call_outputs pattern).
                    let _panic_msg = format!("[tool execution panicked: {e}]");
                    observer
                        .handle_event(AgentEvent::Error(format!("Tool execution panicked: {e}")));

                    // We need the call_id from the original request to push a matching result.
                    // Since join_all preserves order, we can correlate by index.
                    // However, we don't have the call_id here. The sanitize() call
                    // at the top of the next iteration will synthesize missing outputs,
                    // but we should push an explicit one when possible.
                    // For now, the sanitize() safety net handles this case.
                    total_tool_calls += 1;
                }
            }
        }

        metrics.tool_execution_time += tool_exec_start.elapsed();

        // ── Mid-turn compaction (post-tool) ─────────────────────────
        // After tool results are added, check if context grew past budget.
        // This matches Codex's `run_auto_compact` mid-turn pattern.
        if messages.needs_compaction() {
            observer.handle_event(AgentEvent::Turn(TurnEvent::PhaseChanged {
                turn_id: ctx.turn_id.clone(),
                phase: TurnPhase::Compacting,
            }));
            observer.handle_event(AgentEvent::Log(
                "    🔄 Mid-turn compaction: tool outputs grew context past budget...".into(),
            ));
            observer.handle_event(AgentEvent::Compacting { active: true });
            if let Err(e) = messages.compact_history(&ctx.compiler).await {
                observer.handle_event(AgentEvent::Log(format!(
                    "    ⚠️ Mid-turn compaction failed: {e}"
                )));
            }
            metrics.compactions += 1;
            observer.handle_event(AgentEvent::Compacting { active: false });
            // Codex pattern: warn user about accuracy degradation
            observer.handle_event(AgentEvent::Log(
                "    ⚠️ Long threads and compactions can reduce accuracy. Start a new session when possible.".into(),
            ));
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

// `backoff_with_jitter` is now re-exported from `crate::turn_timing`.
// The duplicate implementation that was here has been removed to
// consolidate on a single canonical implementation.

/// Best-effort extraction of target file path from tool call arguments.
///
/// Used by `TurnDiffTracker` to snapshot files before write-tools modify them.
/// Supports common tool argument schemas:
/// - `file_edit` / `file_write`: `{"path": "..."}` or `{"file": "..."}`
/// - `bash`: no specific file target (returns None)
fn extract_target_path(
    arguments: &serde_json::Value,
    workspace_root: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let obj = arguments.as_object()?;

    // Try common field names for file path
    let path_str = obj
        .get("path")
        .or_else(|| obj.get("file"))
        .or_else(|| obj.get("file_path"))
        .or_else(|| obj.get("target"))
        .and_then(|v| v.as_str())?;

    let path = std::path::Path::new(path_str);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(workspace_root.join(path))
    }
}
