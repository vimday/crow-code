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
use crow_probe::VerificationCommand;
use std::path::Path;

use crate::context::ConversationManager;

// ─── Constants ──────────────────────────────────────────────────────

/// Maximum bytes to read from a single file before truncation.
const MAX_FILE_BYTES: u64 = 50 * 1024; // 50 KB

/// Maximum lines to read from a single file.
const MAX_FILE_LINES: usize = 500;

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

pub async fn run_epistemic_loop(
    compiler: &IntentCompiler,
    messages: &mut ConversationManager,
    frozen_root: &Path,
    mcp_manager: Option<&crate::mcp::McpManager>,
    observer: &mut dyn EventHandler,
    file_state_store: std::sync::Arc<crate::file_state::FileStateStore>,
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
        let action_result = compiler
            .compile_action_streaming(&messages.as_messages(), &mut adapter)
            .await;

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

                let file_contents = read_files_to_context(&paths, frozen_root);
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

                execute_recon(&tool, frozen_root, messages, mcp_manager).await;
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

                let subagent = crate::subagent::SubagentWorker::new(compiler.clone());

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

// ─── File Reading ───────────────────────────────────────────────────

/// Read multiple files from the frozen sandbox into a formatted context string.
///
/// Each file is truncated at `MAX_FILE_BYTES` / `MAX_FILE_LINES` (whichever
/// triggers first). A system warning is appended if truncation occurred.
fn read_files_to_context(paths: &[crow_patch::WorkspacePath], frozen_root: &Path) -> String {
    use std::io::{BufRead, BufReader};

    let mut file_contents = String::from("[READ FILES RESULT]\n");

    for path in paths {
        let abs_path = path.to_absolute(frozen_root);
        let file_size = std::fs::metadata(&abs_path).map(|m| m.len()).unwrap_or(0);

        let content = match std::fs::File::open(&abs_path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                let mut text = String::new();
                let mut lines_count = 0;
                let mut bytes_read = 0;
                let mut was_truncated = false;

                for line_res in reader.lines() {
                    match line_res {
                        Ok(line) => {
                            if bytes_read + line.len() > MAX_FILE_BYTES as usize {
                                let allowed = (MAX_FILE_BYTES as usize).saturating_sub(bytes_read);
                                text.push_str(crow_patch::util::safe_truncate(&line, allowed));
                                was_truncated = true;
                                lines_count += 1;
                                break;
                            }
                            text.push_str(&line);
                            text.push('\n');
                            bytes_read += line.len() + 1;
                            lines_count += 1;

                            if lines_count >= MAX_FILE_LINES {
                                // If the file has more data, mark it as truncated
                                if file_size > bytes_read as u64 {
                                    was_truncated = true;
                                }
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }

                if was_truncated {
                    format!(
                        "{}\n\n[SYSTEM WARNING: File truncated. Original size: {} bytes, showing first {} lines only.]",
                        text.trim_end(), file_size, lines_count
                    )
                } else {
                    text.trim_end().to_string()
                }
            }
            Err(_) => "<file not found or unreadable>".into(),
        };

        file_contents.push_str(&format!("--- {} ---\n{}\n\n", path.as_str(), content));
    }

    file_contents.push_str("Please proceed with your task, or read more files if necessary.");
    file_contents
}

// ─── Recon Execution ────────────────────────────────────────────────

/// Translate a `ReconAction` into a safe command invocation, execute it
/// against the frozen sandbox, and push the result into the conversation.
async fn execute_recon(
    tool: &ReconAction,
    frozen_root: &Path,
    messages: &mut ConversationManager,
    mcp_manager: Option<&crate::mcp::McpManager>,
) {
    // Intercept MCP calls and execute via the manager.
    if let ReconAction::McpCall {
        server_name,
        tool_name,
        arguments,
    } = tool
    {
        if let Some(mcp) = mcp_manager {
            match mcp.call(server_name, tool_name, arguments.clone()).await {
                Ok(res) => {
                    let formatted_res = if res.is_error {
                        format!("MCP Error: {:?}", res.content)
                    } else {
                        // Very naive formatter for now
                        format!("{:?}", res.content)
                    };
                    messages.push_recon_result(
                        "mcp_call",
                        &format!("{server_name} / {tool_name}"),
                        &formatted_res,
                    );
                }
                Err(e) => {
                    messages.push_user(format!(
                        "[MCP ERROR]\nFailed to execute {server_name}/{tool_name}: {e:?}"
                    ));
                }
            }
        } else {
            messages.push_user(format!(
                "[MCP ERROR]\nMCP is not enabled or MCP manager unavailable, cannot call {server_name}/{tool_name}"
            ));
        }
        return;
    }

    if let ReconAction::FetchUrl { url } = tool {
        let max_fetch_bytes = 1024 * 50; // max 50KB to protect context

        let client_res = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("crow-code-agent/1.0")
            .build();

        let client = match client_res {
            Ok(c) => c,
            Err(e) => {
                messages.push_user(format!(
                    "[WEB FETCH ERROR]\nFailed to initialize HTTP client: {e:?}"
                ));
                return;
            }
        };

        match client.get(url).send().await {
            Ok(res) => {
                let status = res.status();
                if !status.is_success() {
                    messages.push_user(format!("[WEB FETCH ERROR]\n{url} returned HTTP {status}"));
                } else {
                    if let Some(ct) = res.headers().get(reqwest::header::CONTENT_TYPE) {
                        let ct_str = ct.to_str().unwrap_or("");
                        if !ct_str.contains("text/") && !ct_str.contains("application/json") {
                            messages.push_user(format!("[WEB FETCH ERROR]\nUnsupported Content-Type '{ct_str}'. Only text or json supported."));
                            return;
                        }
                    }

                    match res.text().await {
                        Ok(mut text) => {
                            let truncated = if text.len() > max_fetch_bytes {
                                text.truncate(max_fetch_bytes);
                                format!("{text}...\n\n[SYSTEM WARNING: Response truncated to 50KB]")
                            } else {
                                text
                            };
                            messages.push_recon_result("fetch_url", url, &truncated);
                        }
                        Err(e) => {
                            messages.push_user(format!(
                                "[WEB FETCH ERROR]\nFailed to decode response from {url}: {e:?}"
                            ));
                        }
                    }
                }
            }
            Err(e) => {
                messages.push_user(format!("[WEB FETCH ERROR]\nFailed to fetch {url}: {e:?}"));
            }
        }
        return;
    }

    let (program, args, description) = build_recon_command(tool);

    let v_cmd = VerificationCommand {
        program: program.clone(),
        args,
        cwd: None,
    };
    let exec_config = crow_verifier::ExecutionConfig {
        timeout: std::time::Duration::from_secs(10),
        max_output_bytes: 512 * 1024, // 512KB hard cap for recon
    };

    let result = crow_verifier::executor::execute(
        frozen_root,
        &v_cmd,
        &exec_config,
        &crow_verifier::types::AciConfig::compact(),
        None, // Recon: ephemeral, no cache reuse needed
    )
    .await;

    match result {
        Ok(res) => {
            let tool_name = recon_tool_name(tool);
            messages.push_recon_result(tool_name, &description, &res.test_run.truncated_log);
        }
        Err(e) => {
            messages.push_user(format!(
                "[RECON ERROR]\nFailed to execute `{program}`: {e:?}"
            ));
        }
    }
}

/// Translate a `ReconAction` into `(program, args, description)`.
///
/// Single source of truth — uses the strictest variant from both
/// the serial and MCTS paths:
/// - `wc -l -c --` (always include count flags)
/// - `DirTree` depth clamped to `.min(10)`
/// - Formatted `rg` description for cleaner logs
fn build_recon_command(tool: &ReconAction) -> (String, Vec<String>, String) {
    match tool {
        ReconAction::ListDir { path } => (
            "ls".to_string(),
            vec![
                "-la".to_string(),
                "--".to_string(),
                path.as_str().to_string(),
            ],
            format!("ls -la -- {}", path.as_str()),
        ),
        ReconAction::Search {
            pattern,
            path,
            glob,
        } => {
            let mut a = vec![
                "-rn".to_string(),
                "-e".to_string(), // Explicitly mark pattern so it's not parsed as flag
                pattern.clone(),
            ];
            if let Some(g) = glob {
                a.push("-g".to_string());
                a.push(g.clone());
            }
            a.push("--".to_string()); // Terminate options before path
            if let Some(p) = path {
                a.push(p.as_str().to_string());
            } else {
                a.push(".".to_string());
            }
            let desc = format!(
                "rg -rn -e '{}' {}",
                pattern,
                path.as_ref()
                    .map(crow_patch::WorkspacePath::as_str)
                    .unwrap_or(".")
            );
            ("rg".to_string(), a, desc)
        }
        ReconAction::FileInfo { path } => (
            "file".to_string(),
            vec!["--".to_string(), path.as_str().to_string()],
            format!("file -- {}", path.as_str()),
        ),
        ReconAction::WordCount { path } => (
            "wc".to_string(),
            vec![
                "-l".to_string(),
                "-c".to_string(),
                "--".to_string(),
                path.as_str().to_string(),
            ],
            format!("wc -lc -- {}", path.as_str()),
        ),
        ReconAction::DirTree { path, max_depth } => {
            let depth = max_depth.unwrap_or(3).min(10);
            (
                "tree".to_string(),
                vec![
                    "-L".to_string(),
                    depth.to_string(),
                    "--".to_string(),
                    path.as_str().to_string(),
                ],
                format!("tree -L {} -- {}", depth, path.as_str()),
            )
        }
        ReconAction::McpCall { .. } => {
            unreachable!(
                "McpCall is intercepted and executed via mcp_manager before command building"
            );
        }
        ReconAction::FetchUrl { .. } => {
            unreachable!(
                "FetchUrl is intercepted and executed via reqwest before command building"
            );
        }
    }
}

/// Map a `ReconAction` variant to its tool name string for compression heuristics.
fn recon_tool_name(tool: &ReconAction) -> &'static str {
    match tool {
        ReconAction::ListDir { .. } => "list_dir",
        ReconAction::Search { .. } => "search",
        ReconAction::FileInfo { .. } => "file_info",
        ReconAction::WordCount { .. } => "word_count",
        ReconAction::DirTree { .. } => "dir_tree",
        ReconAction::McpCall { .. } => "mcp_call",
        ReconAction::FetchUrl { .. } => "fetch_url",
    }
}
