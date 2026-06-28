use crate::config::CrowConfig;
use crate::event::{AgentEvent, TurnPhase, ViewMode};
use crate::tui::commands::{execute_shell_command, handle_enter};
use crate::tui::components::{composer::ComposerComponent, history::HistoryComponent};
use crate::tui::history_cell;
use crate::tui::render::render_app;
use crate::tui::state::{self, AppState, TuiMessage};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const CTRL_C_QUIT_WINDOW: Duration = Duration::from_millis(1500);

pub async fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    rx: &mut mpsc::UnboundedReceiver<TuiMessage>,
    tx: &mpsc::UnboundedSender<TuiMessage>,
    cfg: CrowConfig,
    thread_manager: &Arc<crate::thread_manager::ThreadManager>,
) -> Result<()> {
    let mut composer_comp = ComposerComponent::new();
    let mut history_comp = HistoryComponent::new();

    'event_loop: loop {
        // Update terminal title (Codex pattern: workspace + state)
        {
            let title_state = if state.is_streaming {
                "Streaming"
            } else if let Some(ref phase) = state.turn_phase {
                phase.as_str()
            } else if state.is_task_running() {
                "Running"
            } else {
                "Ready"
            };
            let title = format!("🦅 Crow · {} · {title_state}", state.workspace_name);
            let _ = crate::tui::terminal_title::set_terminal_title(&title);
        }

        terminal.draw(|f| render_app(f, state, &mut composer_comp, &mut history_comp))?;

        // Poll for keyboard events
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    // ── Ctrl+C: interrupt or quit ────────────────────────────
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        if state.is_task_running() {
                            // First press while running: interrupt the task
                            if let Some(token) = &state.cancellation {
                                token.cancel();
                            }
                            state.active_action = None;
                            state.push_log("Interrupted.");
                            state.last_ctrl_c = Some(Instant::now());
                        } else if let Some(last) = state.last_ctrl_c {
                            if last.elapsed() < CTRL_C_QUIT_WINDOW {
                                break 'event_loop; // Second Ctrl+C within window: quit
                            } else {
                                state.last_ctrl_c = Some(Instant::now());
                                state.quit_hint_until = Some(Instant::now() + CTRL_C_QUIT_WINDOW);
                            }
                        } else {
                            state.last_ctrl_c = Some(Instant::now());
                            state.quit_hint_until = Some(Instant::now() + CTRL_C_QUIT_WINDOW);
                        }
                        continue;
                    }

                    // ── Ctrl+D: quit immediately ─────────────────────────────
                    if key.code == KeyCode::Char('d')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break 'event_loop;
                    }

                    // ── ESC: interrupt running task (Codex behavior) ─────────
                    // ESC does NOT quit. It interrupts a running agent turn.
                    if key.code == KeyCode::Esc {
                        if state.is_task_running() {
                            if let Some(token) = &state.cancellation {
                                token.cancel();
                            }
                            state.active_action = None;
                            state.push_log("Interrupted.");
                        }
                        // When idle, ESC does nothing (no quit).
                        continue;
                    }

                    // Reset Ctrl+C quit window on any other key
                    state.last_ctrl_c = None;
                    state.quit_hint_until = None;

                    // Dismiss shortcut overlay on any key except `?` or `？`
                    if state.show_shortcuts_overlay && key.code != KeyCode::Char('?') && key.code != KeyCode::Char('？') {
                        state.show_shortcuts_overlay = false;
                    }

                    // ── Shell Command Approval Interception ───────────────────
                    if let crate::tui::state::ApprovalState::PendingCommand(cmd, mut selected_idx) =
                        state.approval_state.clone()
                    {
                        match key.code {
                            KeyCode::Up => {
                                selected_idx = selected_idx.saturating_sub(1);
                                state.approval_state = crate::tui::state::ApprovalState::PendingCommand(cmd, selected_idx);
                            }
                            KeyCode::Down => {
                                selected_idx = (selected_idx + 1).min(2);
                                state.approval_state = crate::tui::state::ApprovalState::PendingCommand(cmd, selected_idx);
                            }
                            // Single-key shortcuts: y=Allow Once, a=Allow Always, n=Reject
                            KeyCode::Char('y') => {
                                state.approval_state = crate::tui::state::ApprovalState::None;
                                state.push_user(format!("!{cmd}"));
                                state.active_action = Some(format!("$ {cmd}"));
                                execute_shell_command(cmd, tx.clone());
                            }
                            KeyCode::Char('a') => {
                                state.approval_state = crate::tui::state::ApprovalState::None;
                                let prefix =
                                    cmd.split_whitespace().next().unwrap_or(&cmd).to_string();
                                state.allowed_safe_patterns.insert(prefix.clone());
                                state.push_log(format!("Whitelist updated: '{prefix}' will auto-execute for this session."));
                                state.push_user(format!("!{cmd}"));
                                state.active_action = Some(format!("$ {cmd}"));
                                execute_shell_command(cmd, tx.clone());
                            }
                            KeyCode::Char('n') | KeyCode::Esc => {
                                state.approval_state = crate::tui::state::ApprovalState::None;
                                state.push_log(format!("Command cancelled: {cmd}"));
                            }
                            KeyCode::Enter => {
                                match selected_idx {
                                    0 => {
                                        // Allow Once
                                        state.approval_state = crate::tui::state::ApprovalState::None;
                                        state.push_user(format!("!{cmd}"));
                                        state.active_action = Some(format!("$ {cmd}"));
                                        execute_shell_command(cmd, tx.clone());
                                    }
                                    1 => {
                                        // Allow Always
                                        state.approval_state = crate::tui::state::ApprovalState::None;
                                        let prefix =
                                            cmd.split_whitespace().next().unwrap_or(&cmd).to_string();
                                        state.allowed_safe_patterns.insert(prefix.clone());
                                        state.push_log(format!("Whitelist updated: '{prefix}' will auto-execute for this session."));
                                        state.push_user(format!("!{cmd}"));
                                        state.active_action = Some(format!("$ {cmd}"));
                                        execute_shell_command(cmd, tx.clone());
                                    }
                                    _ => {
                                        // Reject
                                        state.approval_state = crate::tui::state::ApprovalState::None;
                                        state.push_log(format!("Command cancelled: {cmd}"));
                                    }
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }



                    // ── Global Hotkeys ─────────────────────────────
                    if key.code == KeyCode::PageUp {
                        state.scroll_offset = state.scroll_offset.saturating_add(5);
                        continue;
                    }
                    if key.code == KeyCode::PageDown {
                        state.scroll_offset = state.scroll_offset.saturating_sub(5);
                        continue;
                    }
                    if key.code == KeyCode::Tab {
                        if state.focus == crate::tui::state::Focus::Composer {
                            state.focus = crate::tui::state::Focus::History;
                        } else {
                            state.focus = crate::tui::state::Focus::Composer;
                        }
                        continue;
                    }

                    // ── `?` toggles shortcut overlay (Codex pattern) ─────────
                    if (key.code == KeyCode::Char('?') || key.code == KeyCode::Char('？'))
                        && state.focus == crate::tui::state::Focus::Composer
                        && state.composer.is_empty()
                        && !state.is_task_running()
                    {
                        state.show_shortcuts_overlay = !state.show_shortcuts_overlay;
                        continue;
                    }

                    // ── Route Event to Focused Component ─────────────────────────────
                    use crate::tui::component::Component;
                    let action = match state.focus {
                        crate::tui::state::Focus::Composer => composer_comp.handle_event(&Event::Key(key), state)?,
                        crate::tui::state::Focus::History => history_comp.handle_event(&Event::Key(key), state)?,
                        _ => None,
                    };

                    if let Some(act) = action {
                        match act {
                            crate::tui::component::TuiAction::SubmitCommand(cmd) => {
                                state.composer = cmd;
                                handle_enter(state, tx, &cfg, thread_manager);
                            }
                            crate::tui::component::TuiAction::FocusNext => {
                                state.focus = crate::tui::state::Focus::History;
                            }
                            _ => {}
                        }
                    }
                }
                Event::Paste(text)
                    // Route Paste to the focused component.
                    if state.focus == crate::tui::state::Focus::Composer => {
                        use crate::tui::component::Component;
                        let _ = composer_comp.handle_event(&Event::Paste(text), state);
                    }
                _ => {}
            }
        }

        // ── Process message bus ──────────────────────────────────────────
        while let Ok(msg) = rx.try_recv() {
            match msg {
                TuiMessage::AgentEvent(ev) => {
                    handle_agent_event(state, ev);
                }
                TuiMessage::TurnComplete(success, timing) => {
                    // Flush any pending streaming buffer to the active cell
                    // before committing (throttling may have skipped the last rebuild)
                    if !state.streaming_buffer.is_empty()
                        && state.streaming_buffer.len() > state.last_cell_buffer_len
                    {
                        state.active_cell = Some(Box::new(history_cell::AgentMessageCell {
                            payload: state.streaming_buffer.clone(),
                            is_continuation: false,
                        }));
                    }

                    // Commit active cell to history
                    if let Some(cell) = state.active_cell.take() {
                        state.history.push(cell);
                    }

                    state.active_action = None;
                    state.task_start_time = None;
                    state.turn_phase = None;
                    state.status_indicator = None;

                    let final_tokens = state.streaming_token_estimate;

                    // Reset streaming metrics (Yomi pattern)
                    state.is_streaming = false;
                    state.streaming_token_estimate = 0.0;
                    state.streaming_start_time = None;
                    state.last_cell_update = None;
                    state.last_cell_buffer_len = 0;
                    let was_cancelled = state
                        .cancellation
                        .as_ref()
                        .is_some_and(state::CancellationToken::is_cancelled);
                    state.cancellation = None;

                    if success && !was_cancelled {
                        // Display timing summary in Audit mode (Codex TurnCompleted pattern)
                        let timing_label = if let Some(ref t) = timing {
                            let total_s = t.total_ms as f64 / 1000.0;
                            let ttft = t
                                .ttft_ms
                                .map(|ms| format!("{ms}ms"))
                                .unwrap_or_else(|| "n/a".to_string());
                            let tps_display = if total_s > 0.0 && final_tokens > 0.0 {
                                format!(", {:.1} tok/s", final_tokens / total_s)
                            } else {
                                String::new()
                            };
                            format!(
                                "✅ Done ({total_s:.1}s, {} LLM call(s), TTFT: {ttft}{tps_display})",
                                t.llm_calls
                            )
                        } else {
                            "Done".to_string()
                        };
                        state.push_result(timing_label);

                        if let Some(next_task) = state.task_queue.pop_front() {
                            crate::tui::commands::execute_command_string(
                                state,
                                next_task,
                                tx,
                                &cfg,
                                thread_manager,
                            );
                        }
                    } else if !state.task_queue.is_empty() {
                        let drop_count = state.task_queue.len();
                        state.task_queue.clear();
                        state.push_error(format!(
                            "Pipeline halted. Dropped {drop_count} queued queries."
                        ));
                    }

                    // Refresh git state post-turn in case files were modified
                    crate::tui::app::refresh_git_state(state, &cfg.workspace);
                }
                TuiMessage::SessionComplete => {
                    if let Some(cell) = state.active_cell.take() {
                        state.history.push(cell);
                    }
                    state.active_action = None;
                    state.task_start_time = None;
                    state.is_streaming = false;
                    state.streaming_token_estimate = 0.0;
                    state.streaming_start_time = None;
                    state.cancellation = None;
                    state.status_indicator = None;
                    crate::tui::app::refresh_git_state(state, &cfg.workspace);
                }
                TuiMessage::SwarmStarted(id, task) => {
                    state.active_swarms.push((id, task));
                }
                TuiMessage::SwarmComplete(id, success) => {
                    state
                        .active_swarms
                        .retain(|(active_id, _)| active_id != &id);
                    if success {
                        state.push_result(format!("Swarm worker [{id}] finished."));
                    } else {
                        state.push_error(format!("Swarm worker [{id}] finished."));
                    }
                }
                TuiMessage::Tick => {
                    state.spinner_idx = state.spinner_idx.wrapping_add(1);

                    // We no longer rely on stream_controller for buffered rendering.
                    // The live active_cell is natively rendered by the TUI render loop.

                    // Auto-clear expired status messages (Yomi pattern)
                    state.check_status_timeout();

                    // Auto-expire quit hint (Codex pattern)
                    if state.quit_hint_until.is_some_and(|t| Instant::now() >= t) {
                        state.quit_hint_until = None;
                    }

                    if let Some(start) = state.task_start_time {
                        if start.elapsed() > Duration::from_secs(180) {
                            state.push_error("Network response or task execution is taking over 3 minutes. Is it hanging? Press ESC to force-interrupt.");
                            // Reset timer to warn again in 3 minutes if still stuck
                            state.task_start_time = Some(Instant::now());
                        }
                    }

                    // Best-effort draft persistence
                    if state.spinner_idx.is_multiple_of(8) {
                        // ~ once per second (120ms * 8 tick)
                        let draft_path =
                            std::path::Path::new(&cfg.workspace).join(".crow/logs/draft.txt");
                        if !state.composer.is_empty() {
                            let _ = std::fs::write(&draft_path, &state.composer);
                        } else {
                            let _ = std::fs::remove_file(&draft_path);
                        }
                    }
                }
                TuiMessage::Quit => break 'event_loop,
            }
        }
    }
    Ok(())
}

fn handle_agent_event(state: &mut AppState, event: AgentEvent) {
    state.task_start_time = Some(Instant::now());
    match event {
        AgentEvent::Turn(turn_ev) => {
            use crate::event::TurnEvent;
            match turn_ev {
                TurnEvent::Started { turn_id } => {
                    if state.view_mode == ViewMode::Audit {
                        state.push_log(format!("Turn started: {turn_id}"));
                    }
                }
                TurnEvent::Completed {
                    turn_id, success, ..
                } => {
                    if state.view_mode == ViewMode::Audit {
                        let status = if success { "✓" } else { "✘" };
                        state.push_log(format!("{status} Turn completed: {turn_id}"));
                    }
                }
                TurnEvent::Aborted { turn_id, reason } => {
                    state.push_error(format!("Turn aborted [{turn_id}]: {reason}"));
                }
                TurnEvent::PhaseChanged { phase, .. } => {
                    let label = match &phase {
                        TurnPhase::Materializing => "📦 Materializing".to_string(),
                        TurnPhase::BuildingRepoMap => "🗺️ Building repo map".to_string(),
                        TurnPhase::Compacting => "🔄 Compacting context".to_string(),
                        TurnPhase::EpistemicLoop { step, max_steps } => {
                            format!("🧠 Thinking (step {step}/{max_steps})")
                        }
                        TurnPhase::CruciblePreflight => "🔍 Preflight checks".to_string(),
                        TurnPhase::CrucibleVerification { attempt } => {
                            format!("🧪 Verifying (attempt {attempt})")
                        }
                        TurnPhase::Applying => "⚡ Applying changes".to_string(),
                        TurnPhase::Complete => "✅ Complete".to_string(),
                    };
                    state.active_action = Some(label.clone());
                    state.turn_phase = Some(label);
                }
                TurnEvent::DiffGenerated {
                    diff_text,
                    files_changed,
                    ..
                } => {
                    if !diff_text.is_empty() {
                        state.push_diff(&diff_text);
                        state.push_log(format!("{files_changed} file(s) changed this turn."));
                    }
                }
            }
        }
        AgentEvent::Thinking(_, _) => {
            state.active_action = Some("Thinking...".into());
            state.status_indicator = Some(state::StatusIndicatorState {
                header: "Thinking...".into(),
                details: None,
                details_max_lines: 3,
                progress_pct: None,
            });
            // Start a fresh streaming session for this turn
            state.stream_controller.start();
            // Reset streaming buffer for the new turn
            state.streaming_buffer.clear();
            // Start streaming metrics (Yomi InfoBar pattern)
            state.is_streaming = true;
            state.streaming_token_estimate = 0.0;
            state.streaming_start_time = Some(Instant::now());
            state.last_cell_update = None;
            state.last_cell_buffer_len = 0;
        }
        AgentEvent::StreamChunk(chunk) => {
            // Accumulate into streaming buffer
            state.streaming_buffer.push_str(&chunk);
            state.streaming_token_estimate += AppState::estimate_tokens(&chunk);

            // Throttled cell rebuild: only create a new AgentMessageCell if
            // ≥100 chars have been added since the last rebuild, OR ≥50ms
            // has elapsed. This avoids cloning the entire buffer on every
            // single token arrival.
            let chars_since = state.streaming_buffer.len().saturating_sub(state.last_cell_buffer_len);
            let elapsed_ok = state
                .last_cell_update
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(50));

            if chars_since >= 100 || elapsed_ok {
                state.active_cell = Some(Box::new(history_cell::AgentMessageCell {
                    payload: state.streaming_buffer.clone(),
                    is_continuation: false,
                }));
                state.last_cell_update = Some(Instant::now());
                state.last_cell_buffer_len = state.streaming_buffer.len();
            }

            state.scroll_offset = 0; // Force scroll to bottom on new content
        }
        AgentEvent::Markdown(md) => {
            state.streaming_buffer = md.clone();
            state.active_cell = Some(Box::new(history_cell::AgentMessageCell {
                payload: md,
                is_continuation: false,
            }));
            state.scroll_offset = 0;
        }
        AgentEvent::Log(msg) => {
            // Route diff output to DiffCell for syntax-highlighted rendering
            if let Some(diff_content) = msg.strip_prefix("__DIFF__") {
                state.push_diff(diff_content);
            } else {
                state.push_log(msg);
            }
        }
        AgentEvent::ActionStart(desc) => {
            state.active_action = Some(desc.clone());
            state.status_indicator = Some(state::StatusIndicatorState {
                header: desc,
                details: None,
                details_max_lines: 3,
                progress_pct: None,
            });
        }
        AgentEvent::ActionComplete(desc) => {
            state.push_action(desc);
        }
        AgentEvent::ToolCallStarted {
            tool_name,
            is_read_only,
            ..
        } => {
            let kind = if is_read_only { "read" } else { "write" };
            let header = format!("{tool_name} ({kind})");
            state.active_action = Some(header.clone());
            state.status_indicator = Some(state::StatusIndicatorState {
                header,
                details: None,
                details_max_lines: 3,
                progress_pct: None,
            });
        }
        AgentEvent::ToolCallCompleted {
            tool_name,
            duration_ms,
            output_bytes,
            is_error,
            retry_count,
            from_cache,
            preview,
            ..
        } => {
            // Push a polished tool-call card to history (replaces the
            // flat ActionCell line). Preview is one-line, dim, wrapped.
            state.push_tool_card(
                tool_name,
                duration_ms,
                output_bytes,
                is_error,
                preview,
                retry_count,
                from_cache,
            );
        }
        AgentEvent::ReadFiles(paths) => {
            if state.view_mode != ViewMode::Focus {
                let display = if paths.len() <= 3 {
                    paths.join(", ")
                } else {
                    format!("{}, ... ({} files)", paths[..2].join(", "), paths.len())
                };
                state.push_evidence(format!("Read {display}"));
            }
        }
        AgentEvent::ReconStart(desc) => {
            let header = format!("Recon: {desc}");
            state.active_action = Some(header.clone());
            state.status_indicator = Some(state::StatusIndicatorState {
                header,
                details: Some(desc),
                details_max_lines: 3,
                progress_pct: None,
            });
        }
        AgentEvent::DelegateStart(id, task) => {
            state.active_action = Some(format!("Delegating: {task}"));
            state.active_swarms.push((id, task));
        }
        AgentEvent::DelegateComplete(id, _success) => {
            state
                .active_swarms
                .retain(|(active_id, _)| active_id != &id);
        }
        AgentEvent::PlanSubmitted(plan) => {
            if !plan.operations.is_empty() {
                state.push_action(format!("{} operations planned", plan.operations.len()));
            }
        }
        AgentEvent::CruciblePreflight(msg) => {
            state.active_action = Some(format!("Verifying: {msg}"));
        }
        AgentEvent::Error(err) => {
            state.push_error(err);
            state.active_action = None;
            state.task_start_time = None;
            state.status_indicator = None;
        }
        AgentEvent::PhasedError {
            phase,
            error,
            is_recoverable,
        } => {
            if is_recoverable {
                // Recoverable errors: show as transient status (yomi pattern)
                state.show_status(
                    state::StatusMessage::warn(format!("{phase} error (will retry): {error}")),
                    5000,
                );
            } else {
                // Non-recoverable: add to history and stop
                state.push_error(format!("[{phase}] {error}"));
                state.active_action = None;
                state.task_start_time = None;
            }
        }
        // ── High-granularity events (Yomi-inspired) ─────────────────────
        AgentEvent::TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            context_window,
        } => {
            // Update context window usage for status bar (Yomi pattern)
            state.ctx_usage = Some((total_tokens, context_window));
            // Accumulate session-level totals for /cost
            state.cumulative_prompt_tokens += prompt_tokens;
            state.cumulative_completion_tokens += completion_tokens;
        }
        AgentEvent::StateChanged { from, to } => {
            if state.view_mode == ViewMode::Audit {
                state.push_log(format!("State: {from} → {to}"));
            }
        }
        AgentEvent::Retrying {
            attempt,
            max_attempts,
            reason,
        } => {
            state.active_action = Some(format!("Retrying ({attempt}/{max_attempts})… {reason}"));
            // Show timed warning in status bar (Yomi pattern)
            state.show_status(
                state::StatusMessage::warn(format!(
                    "Retrying ({attempt}/{max_attempts}): {reason}"
                )),
                5000,
            );
        }
        AgentEvent::Compacting { active } => {
            if active {
                state.active_action = Some("Compacting context…".into());
                // Indeterminate progress: show the bar in animated mode at 33%
                // until we get a real percentage from the compactor.
                state.status_indicator = Some(state::StatusIndicatorState {
                    header: "Compacting context…".into(),
                    details: Some("Summarizing older turns to free up context".into()),
                    details_max_lines: 3,
                    progress_pct: Some(33),
                });
            } else {
                state.active_action = None;
                state.push_action("Context compaction complete");
            }
        }
        AgentEvent::ToolProgress {
            tool_id: _,
            message,
        } => {
            state.active_action = Some(message);
        }
    }
}
