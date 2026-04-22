//! Shared epistemic engine for the autonomous crucible loop.
//!
//! Extracts the common logic for the ReadFiles / Recon / SubmitPlan
//! interaction cycle used by both the serial crucible (`run_dry_run`)
//! and the MCTS crucible (`run_mcts_crucible`).
//!
//! # Design Principles
//!
//! - **Single source of truth** for recon command translation, file
//!   reading, and epistemic loop control.
//! - **Strict safety**: all paths are resolved against the frozen sandbox,
//!   all commands are allowlisted, all output is budget-capped.

use anyhow::Result;
use crow_brain::IntentCompiler;
use crow_patch::{AgentAction, IntentPlan, ReconAction};
use std::path::Path;

use crate::context::ConversationManager;

// ─── Constants ──────────────────────────────────────────────────────

/// Maximum bytes from a recon result before truncation at context level.
/// Separate from the execution-level cap (512KB) in the verifier — this
/// prevents oversized tool output from blowing out the conversation window.
const MAX_RECON_CONTEXT_BYTES: usize = 100 * 1024; // 100 KB

/// Maximum epistemic steps before bailing out.
const MAX_EPISTEMIC_STEPS: usize = 30;

// ─── Epistemic Loop ─────────────────────────────────────────────────

/// Run the epistemic loop until a `SubmitPlan` is produced.
///
/// Drives the ReadFiles → Recon → SubmitPlan cycle, feeding tool
/// results back into the conversation context. Returns the compiled
/// `IntentPlan` when the LLM submits one.
///
/// Used by both the serial crucible and MCTS pre-exploration.
use crate::event::{AgentEvent, EventHandler};

#[allow(clippy::too_many_arguments)]
pub async fn run_epistemic_loop(
    compiler: &IntentCompiler,
    messages: &mut ConversationManager,
    frozen_root: &Path,
    mcp_manager: Option<&crate::mcp::McpManager>,
    observer: &mut dyn EventHandler,
    file_state_store: std::sync::Arc<crate::file_state::FileStateStore>,
    tool_registry: std::sync::Arc<crow_tools::ToolRegistry>,
    permissions: std::sync::Arc<crow_tools::PermissionEnforcer>,
) -> Result<IntentPlan> {
    let mut epistemic_step = 0;
    // Track action signatures to detect duplicate recon loops.
    let mut seen_actions: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut consecutive_dupes = 0u32;
    // Track subagent delegation depth structurally (codex pattern) instead of via prompt text.
    let mut delegation_count = 0usize;

    loop {
        epistemic_step += 1;
        if epistemic_step > MAX_EPISTEMIC_STEPS {
            anyhow::bail!(
                "Epistemic loop exceeded {MAX_EPISTEMIC_STEPS} steps without producing a SubmitPlan. Aborting."
            );
        }

        // ── Cancellation check ──────────────────────────────────────
        // If the user pressed ESC/Ctrl+C, abort the loop gracefully
        // by returning an empty plan (no-op) so the turn exits cleanly.
        if observer.is_cancelled() {
            observer.handle_event(AgentEvent::Log("Turn cancelled by user.".into()));
            return Ok(IntentPlan {
                base_snapshot_id: crow_patch::SnapshotId("cancelled".into()),
                rationale: String::new(),
                is_partial: false,
                confidence: crow_patch::Confidence::None,
                requires_mcts: false,
                operations: vec![],
            });
        }

        observer.handle_event(AgentEvent::StateChanged {
            from: "WaitingForInput".into(),
            to: "Streaming".into(),
        });
        observer.handle_event(AgentEvent::Thinking(
            epistemic_step as u32,
            MAX_EPISTEMIC_STEPS as u32,
        ));

        struct StreamAdapter<'a>(&'a mut dyn EventHandler);
        impl crow_brain::compiler::StreamObserver for StreamAdapter<'_> {
            fn on_chunk(&mut self, chunk: &str) {
                self.0
                    .handle_event(AgentEvent::StreamChunk(chunk.to_string()));
            }
        }

        let mut adapter = StreamAdapter(observer);
        let action_result = {
            let mut retry_count = 0u32;
            const MAX_LLM_RETRIES: u32 = 3;
            loop {
                match compiler
                    .compile_action_streaming(&messages.as_messages(), &mut adapter)
                    .await
                {
                    Ok(action) => break Ok(action),
                    Err(crow_brain::compiler::CompilerError::PromptFailed(ref brain_err))
                        if brain_err.is_retryable() && retry_count < MAX_LLM_RETRIES =>
                    {
                        retry_count += 1;
                        let backoff_secs = 2u64.pow(retry_count);
                        let obs = &mut *adapter.0;
                        obs.handle_event(AgentEvent::Retrying {
                            attempt: retry_count,
                            max_attempts: MAX_LLM_RETRIES,
                            reason: format!("Transient LLM error: {brain_err}"),
                        });
                        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    }
                    Err(e) => break Err(e),
                }
            }
        };

        let action = action_result.map_err(|e| anyhow::anyhow!("Compilation failed: {e:?}"))?;

        // If it's a SubmitPlan, intercept to enforce hallucination guard before returning.
        if let AgentAction::SubmitPlan { plan } = action {
            // Anti-Hallucination Guard: Ensure all modified files have been recorded.
            if let Some(bad_path) = plan.operations.iter().find_map(|op| {
                if let crow_patch::EditOp::Modify { path, .. } = op {
                    if !file_state_store.has_recorded(path.as_str()) {
                        return Some(path.as_str().to_string());
                    }
                }
                None
            }) {
                observer.handle_event(AgentEvent::Log(format!(
                    "    ⚠️  Hallucination Guard: Attempted to modify unread file: `{bad_path}`. Rejecting plan."
                )));
                messages.push_assistant(serde_json::to_string(&AgentAction::SubmitPlan { plan })?);
                messages.push_user(format!(
                    "[SYSTEM: HALLUCINATION GUARD]\n\
                     You attempted to modify the file `{bad_path}`, but you have not read it during this turn.\n\
                     You MUST use the `read_files` action to examine the current contents of a file BEFORE you submit a patch for it.\n\
                     Output a `read_files` action first to read `{bad_path}`."
                ));
                continue;
            }

            observer.handle_event(AgentEvent::StateChanged {
                from: "Streaming".into(),
                to: "PlanReady".into(),
            });
            observer.handle_event(AgentEvent::PlanSubmitted(plan.clone()));
            return Ok(plan);
        }

        // ── Duplicate Action Detection ──
        let dedup_key = action_dedup_key(&action);
        if !seen_actions.insert(dedup_key) {
            consecutive_dupes += 1;
            if consecutive_dupes >= 2 {
                observer.handle_event(AgentEvent::Log(format!(
                    "    ⚠️  Duplicate action detected ({} times). Nudging agent to submit plan...",
                    consecutive_dupes + 1
                )));
                messages.push_assistant(serde_json::to_string(&action)?);
                messages.push_user(
                    "[SYSTEM: DUPLICATE ACTION DETECTED]\n\
                     You have already performed this exact action. The result is already in your context above.\n\
                     Do NOT repeat the same action. You have enough information.\n\
                     If the user's request does not require file changes, submit a plan with an EMPTY operations array \
                     and put your response text in the rationale field.\n\
                     Output a submit_plan action NOW."
                        .to_string(),
                );
                continue;
            }
        } else {
            consecutive_dupes = 0;
        }

        // Track the agent's action in conversation history.
        messages.push_assistant(serde_json::to_string(&action)?);

        observer.handle_event(AgentEvent::StateChanged {
            from: "Streaming".into(),
            to: "ExecutingTool".into(),
        });
        match action {
            AgentAction::ReadFiles { paths, rationale } => {
                let path_strs: Vec<String> = paths.iter().map(|p| p.as_str().to_string()).collect();
                observer.handle_event(AgentEvent::ReadFiles(path_strs.clone()));
                observer.handle_event(AgentEvent::Log(format!("       Rationale: {rationale}")));

                let ctx = crow_tools::ToolContext {
                    frozen_root,
                    permissions: &permissions,
                };
                
                let file_contents = match tool_registry.execute(
                    "read_files",
                    serde_json::json!({ "paths": paths }),
                    &ctx,
                ).await {
                    Ok(res) => res,
                    Err(e) => format!("[READ FILES ERROR]\nFailed to read files: {e:?}"),
                };

                let path_strings: Vec<String> =
                    paths.iter().map(|s| s.as_str().to_string()).collect();

                // Record state to pass the hallucination guard for future modifications.
                for p in paths.iter() {
                    file_state_store.record(p.as_str(), 1);
                }

                messages.push_file_read(&path_strings, file_contents);
            }
            AgentAction::Recon { tool, rationale } => {
                observer.handle_event(AgentEvent::ReconStart(format!("{tool:?}")));
                observer.handle_event(AgentEvent::Log(format!("       Rationale: {rationale}")));

                let tool_name = match tool {
                    ReconAction::ListDir { .. } => "list_dir",
                    ReconAction::Search { .. } => "search",
                    ReconAction::FileInfo { .. } => "file_info",
                    ReconAction::WordCount { .. } => "word_count",
                    ReconAction::DirTree { .. } => "dir_tree",
                    ReconAction::FetchUrl { .. } => "fetch_url",
                    ReconAction::McpCall { .. } => "mcp_call",
                };
                // MCP is still handled via manager for now
                if let ReconAction::McpCall { server_name, tool_name: mcp_tool, arguments } = &tool {
                    if let Some(mcp) = mcp_manager {
                        match mcp.call(server_name, mcp_tool, arguments.clone()).await {
                            Ok(res) => {
                                let formatted_res = if res.is_error {
                                    format!("MCP Error: {:?}", res.content)
                                } else {
                                    format!("{:?}", res.content)
                                };
                                messages.push_recon_result(
                                    "mcp_call",
                                    &format!("{server_name} / {mcp_tool}"),
                                    &formatted_res,
                                );
                            }
                            Err(e) => {
                                messages.push_user(format!(
                                    "[MCP ERROR]\nFailed to execute {server_name}/{mcp_tool}: {e:?}"
                                ));
                            }
                        }
                    } else {
                        messages.push_user(
                            "[MCP ERROR]\nMCP is not enabled or MCP manager unavailable".to_string()
                        );
                    }
                    continue;
                }

                let ctx = crow_tools::ToolContext {
                    frozen_root,
                    permissions: &permissions,
                };

                let args = match tool {
                    ReconAction::ListDir { path } => serde_json::json!({ "path": path }),
                    ReconAction::Search { pattern, path, glob } => serde_json::json!({ "pattern": pattern, "path": path, "glob": glob }),
                    ReconAction::FileInfo { path } => serde_json::json!({ "path": path }),
                    ReconAction::WordCount { path } => serde_json::json!({ "path": path }),
                    ReconAction::DirTree { path, max_depth } => serde_json::json!({ "path": path, "max_depth": max_depth }),
                    ReconAction::FetchUrl { url } => serde_json::json!({ "url": url }),
                    ReconAction::McpCall { .. } => unreachable!(),
                };

                match tool_registry.execute(tool_name, args, &ctx).await {
                    Ok(mut res) => {
                        if res.len() > MAX_RECON_CONTEXT_BYTES {
                            res.truncate(MAX_RECON_CONTEXT_BYTES);
                            res.push_str("\n\n[SYSTEM WARNING: Recon output truncated to 100KB]");
                        }
                        messages.push_recon_result(tool_name, "Tool Execution", &res);
                    }
                    Err(e) => {
                        messages.push_user(format!(
                            "[RECON ERROR]\nFailed to execute {tool_name}: {e:?}"
                        ));
                    }
                }
            }
            AgentAction::SubmitPlan { .. } => {
                unreachable!("SubmitPlan is intercepted before push_assistant")
            }
            AgentAction::DelegateTask {
                task,
                focus_paths,
                rationale,
            } => {
                observer.handle_event(AgentEvent::DelegateStart(task.clone()));
                observer.handle_event(AgentEvent::Log(format!("       Rationale: {rationale}")));

                if delegation_count >= 3 {
                    observer.handle_event(AgentEvent::Log(
                        "    ⚠️ Subagent recursion limit reached. Halting delegation.".into(),
                    ));
                    messages.push_user("[SYSTEM] Subagent recursion limit exceeded (max 3). You must resolve the task yourself without delegating further.".to_string());
                    continue;
                }

                delegation_count += 1;

                observer.handle_event(AgentEvent::ActionStart(
                    "Spawning isolated Subagent Worker runtime".into(),
                ));

                let subagent = crate::subagent::SubagentWorker::new(
                    crate::subagent::AgentRole::Explorer, 
                    compiler.clone(), 
                    crate::registry::TaskRegistry::new(),
                    tool_registry.clone(),
                    permissions.clone(),
                );

                let sys_msgs = messages
                    .as_messages()
                    .iter()
                    .filter(|m| m.role == crow_brain::ChatRole::System)
                    .cloned()
                    .collect();

                match Box::pin(subagent.execute(
                    &task,
                    &focus_paths,
                    &rationale,
                    sys_msgs,
                    frozen_root,
                    mcp_manager,
                    observer,
                ))
                .await
                {
                    Ok(sub_plan) => {
                        observer.handle_event(AgentEvent::ActionComplete(
                            "Subagent completed. Returning IntentPlan to Architect.".into(),
                        ));
                        messages.push_user(format!(
                            "[SYSTEM: SUBAGENT DELEGATION COMPLETE]\n\
                            The subagent successfully returned an IntentPlan.\n\
                            Subagent Rationale / Findings: {}\n\
                            Proposed Operations: {} operations.\n\n\
                            (If operations were proposed, you may copy them precisely into your own final IntentPlan SubmitPlan if you agree. Or use the findings to continue your orchestration.)",
                            sub_plan.rationale, sub_plan.operations.len()
                        ));
                    }
                    Err(e) => {
                        observer.handle_event(AgentEvent::Error(format!(
                            "Subagent crashed/failed: {e}"
                        )));
                        messages.push_user(format!(
                            "[SYSTEM: SUBAGENT FAILURE]\n\
                            The subagent failed to complete the delegation task.\n\
                            Error: {e}\n\
                            You must rethink your strategy."
                        ));
                    }
                }
            }
        }
    }
}

/// Compute a deduplication key for an action (ignoring rationale text).
fn action_dedup_key(action: &AgentAction) -> String {
    match action {
        AgentAction::ReadFiles { paths, .. } => {
            let mut sorted: Vec<&str> = paths
                .iter()
                .map(crow_patch::WorkspacePath::as_str)
                .collect();
            sorted.sort();
            format!("read:{}", sorted.join(","))
        }
        AgentAction::Recon { tool, .. } => {
            format!("recon:{tool:?}")
        }
        AgentAction::SubmitPlan { .. } => "submit".to_string(),
        AgentAction::DelegateTask { task, .. } => {
            let digest = crow_patch::sha256_hex(task.as_bytes());
            format!("delegate:{}", &digest[0..10])
        }
    }
}

