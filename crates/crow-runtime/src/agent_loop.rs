//! Native tool-calling agent loop.
//!
//! Replaces the legacy `epistemic.rs` loop (custom AgentAction JSON parsing)
//! with a Yomi-inspired streaming tool-call state machine. The agent:
//!
//! 1. Sends messages + tool definitions to the LLM provider via streaming
//! 2. Collects response: text chunks + tool_call requests
//! 3. If no tool_calls → conversation complete, return
//! 4. For each tool_call → execute (in parallel) via ToolRegistry
//! 5. Append tool results as tool-role messages
//! 6. Loop back to step 1
//!
//! This architecture matches how Codex, Claude Code, and Yomi operate,
//! enabling the agent to naturally decide which tools to invoke without
//! requiring custom JSON output formatting.

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

use crate::context::ConversationManager;
use crate::event::{AgentEvent, EventHandler};

// ─── Constants ──────────────────────────────────────────────────────

/// Maximum agent loop iterations before bailing out.
const MAX_AGENT_STEPS: usize = 40;

/// Maximum output bytes from a tool result before truncation.
const MAX_TOOL_OUTPUT_BYTES: usize = 100 * 1024; // 100 KB

/// Maximum number of tool calls to execute per response.
const MAX_TOOL_CALLS_PER_TURN: usize = 20;

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
/// LLM Response → Parse tool_calls → Execute tools → Feed results → LLM Response → ...
/// ```
///
/// Returns when the LLM responds with text only (no tool calls).
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop(
    compiler: Arc<crow_brain::IntentCompiler>,
    messages: &mut ConversationManager,
    workspace_root: &Path,
    tool_registry: Arc<crow_tools::ToolRegistry>,
    permissions: Arc<crow_tools::PermissionEnforcer>,
    file_state: Arc<crow_tools::FileStateStore>,
    background_manager: Arc<crow_tools::BackgroundProcessManager>,
    subagent_delegator: Option<Arc<dyn crow_tools::SubagentDelegator>>,
    mut observer: &mut dyn EventHandler,
) -> Result<AgentLoopResult> {
    let mut step = 0;
    let mut total_tool_calls = 0usize;

    // Get tool definitions from the registry (cached for the duration of the loop)
    let tool_defs = tool_registry.tool_definitions();

    loop {
        step += 1;
        if step > MAX_AGENT_STEPS {
            anyhow::bail!(
                "Agent loop exceeded {MAX_AGENT_STEPS} steps without completing. Aborting."
            );
        }

        // ── Cancellation check ──────────────────────────────────────
        if observer.is_cancelled() {
            observer.handle_event(AgentEvent::Log("Turn cancelled by user.".into()));
            return Ok(AgentLoopResult {
                final_text: String::new(),
                tool_call_count: total_tool_calls,
            });
        }

        observer.handle_event(AgentEvent::StateChanged {
            from: "WaitingForInput".into(),
            to: "Streaming".into(),
        });
        observer.handle_event(AgentEvent::Thinking(step as u32, MAX_AGENT_STEPS as u32));

        // ── Stream LLM response with tools ──────────────────────────
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
            const MAX_LLM_RETRIES: u32 = 3;

            let result = loop {
                match compiler.client()
                    .generate_streaming_with_tools(
                        &messages.as_messages(),
                        &tool_defs,
                        Some(&mut adapter),
                    )
                    .await
                {
                    Ok(resp) => break Ok(resp),
                    Err(ref brain_err) if brain_err.is_retryable() && retry_count < MAX_LLM_RETRIES => {
                        retry_count += 1;
                        let backoff_secs = 2u64.pow(retry_count);
                        adapter.0.handle_event(AgentEvent::Retrying {
                            attempt: retry_count,
                            max_attempts: MAX_LLM_RETRIES,
                            reason: format!("Transient LLM error: {brain_err}"),
                        });
                        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    }
                    Err(e) => break Err(e),
                }
            };

            // Reclaim observer from adapter — adapter is dropped here
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

            return Ok(AgentLoopResult {
                final_text: response_text,
                tool_call_count: total_tool_calls,
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
        let tc_requests: Vec<crow_brain::ToolCallRequest> = tool_calls.iter().map(|tc| (*tc).clone()).collect();
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

        // ── Execute tool calls concurrently ─────────────────────────
        let mut tasks = Vec::with_capacity(calls_to_execute.len());
        for tc in calls_to_execute {
            let registry = Arc::clone(&tool_registry);
            let tc_id = tc.id.clone();
            let tc_name = tc.name.clone();
            let tc_args = tc.arguments.clone();
            let root = workspace_root.to_path_buf();
            let perms = Arc::clone(&permissions);

            let fs = Arc::clone(&file_state);
            let bgm = Arc::clone(&background_manager);
            let delegator = subagent_delegator.clone();
            tasks.push(tokio::spawn(async move {
                let ctx = crow_tools::ToolContext {
                    workspace_root: &root,
                    permissions: &perms,
                    file_state: Some(fs),
                    background_manager: Some(bgm),
                    subagent_delegator: delegator,
                };

                let timeout = std::time::Duration::from_secs(120);
                let result = tokio::time::timeout(
                    timeout,
                    registry.execute(&tc_name, tc_args, &ctx),
                ).await;

                let output = match result {
                    Ok(Ok(out)) => out,
                    Ok(Err(e)) => crow_tools::ToolOutput::error(format!("Tool execution error: {e}")),
                    Err(_) => crow_tools::ToolOutput::error(format!("Tool '{tc_name}' timed out after 120s")),
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
                        let truncated = crow_patch::safe_truncate(&content, MAX_TOOL_OUTPUT_BYTES);
                        content = format!("{truncated}\n\n[SYSTEM WARNING: Tool output truncated to 100KB]");
                    }

                    // Safe preview for the event (avoid UTF-8 boundary panics)
                    let preview = crow_patch::safe_truncate(&content, 120);
                    observer.handle_event(AgentEvent::ActionComplete(
                        format!("{tc_name}: {preview}"),
                    ));

                    if output.is_error {
                        observer.handle_event(AgentEvent::Log(
                            format!("    ⚠️ Tool '{tc_name}' returned error"),
                        ));
                    }

                    // Push tool result into conversation
                    messages.push_tool_result(&tc_id, &content);
                }
                Err(e) => {
                    observer.handle_event(AgentEvent::Error(
                        format!("Tool execution panicked: {e}"),
                    ));
                }
            }
        }

        // ── Proactive Mid-turn Compaction ───────────────────────────
        // Mirroring Codex's proactive `run_auto_compact` mid-turn to prevent 
        // runaway tool outputs from blowing out the context window.
        if messages.needs_compaction() {
            observer.handle_event(AgentEvent::Log("    🔄 Context window nearing limit, running mid-turn compaction...".into()));
            observer.handle_event(AgentEvent::Compacting { active: true });
            if let Err(e) = messages.compact_history(&compiler).await {
                observer.handle_event(AgentEvent::Log(format!("    ⚠️ Compaction failed: {e}")));
            }
            observer.handle_event(AgentEvent::Compacting { active: false });
        }
    }
}
