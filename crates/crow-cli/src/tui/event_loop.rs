use crate::config::CrowConfig;
use crate::event::{AgentEvent, ViewMode};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{backend::CrosstermBackend, Terminal};
use crate::tui::render::render_app;
use crate::tui::state::{self, AppState, Cell, CellKind, TuiMessage};
use crate::tui::components::{composer::ComposerComponent, history::HistoryComponent};
use crate::tui::commands::{execute_shell_command, handle_enter};
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

    loop {
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
                            state.history.push(Cell {
                                kind: CellKind::Log,
                                payload: "Interrupted.".into(),
                            });
                            state.last_ctrl_c = Some(Instant::now());
                        } else if let Some(last) = state.last_ctrl_c {
                            if last.elapsed() < CTRL_C_QUIT_WINDOW {
                                break; // Second Ctrl+C within window: quit
                            } else {
                                state.last_ctrl_c = Some(Instant::now());
                                state.history.push(Cell {
                                    kind: CellKind::Log,
                                    payload: "Press Ctrl+C again to quit.".into(),
                                });
                            }
                        } else {
                            state.last_ctrl_c = Some(Instant::now());
                            state.history.push(Cell {
                                kind: CellKind::Log,
                                payload: "Press Ctrl+C again to quit.".into(),
                            });
                        }
                        continue;
                    }

                    // ── Ctrl+D: quit immediately ─────────────────────────────
                    if key.code == KeyCode::Char('d')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }

                    // ── ESC: interrupt running task (Codex behavior) ─────────
                    // ESC does NOT quit. It interrupts a running agent turn.
                    if key.code == KeyCode::Esc {
                        if state.is_task_running() {
                            if let Some(token) = &state.cancellation {
                                token.cancel();
                            }
                            state.active_action = None;
                            state.history.push(Cell {
                                kind: CellKind::Log,
                                payload: "Interrupted.".into(),
                            });
                        }
                        // When idle, ESC does nothing (no quit).
                        continue;
                    }

                    // Reset Ctrl+C quit window on any other key
                    state.last_ctrl_c = None;

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
                                state.history.push(Cell {
                                    kind: CellKind::User,
                                    payload: format!("!{cmd}"),
                                });
                                state.active_action = Some(format!("$ {cmd}"));
                                execute_shell_command(cmd, tx.clone());
                            }
                            KeyCode::Char('a') => {
                                state.approval_state = crate::tui::state::ApprovalState::None;
                                let prefix =
                                    cmd.split_whitespace().next().unwrap_or(&cmd).to_string();
                                state.allowed_safe_patterns.insert(prefix.clone());
                                state.history.push(Cell {
                                    kind: CellKind::Log,
                                    payload: format!("Whitelist updated: '{prefix}' will auto-execute for this session."),
                                });
                                state.history.push(Cell {
                                    kind: CellKind::User,
                                    payload: format!("!{cmd}"),
                                });
                                state.active_action = Some(format!("$ {cmd}"));
                                execute_shell_command(cmd, tx.clone());
                            }
                            KeyCode::Char('n') | KeyCode::Esc => {
                                state.approval_state = crate::tui::state::ApprovalState::None;
                                state.history.push(Cell {
                                    kind: CellKind::Log,
                                    payload: format!("Command cancelled: {cmd}"),
                                });
                            }
                            KeyCode::Enter => {
                                match selected_idx {
                                    0 => {
                                        // Allow Once
                                        state.approval_state = crate::tui::state::ApprovalState::None;
                                        state.history.push(Cell {
                                            kind: CellKind::User,
                                            payload: format!("!{cmd}"),
                                        });
                                        state.active_action = Some(format!("$ {cmd}"));
                                        execute_shell_command(cmd, tx.clone());
                                    }
                                    1 => {
                                        // Allow Always
                                        state.approval_state = crate::tui::state::ApprovalState::None;
                                        let prefix =
                                            cmd.split_whitespace().next().unwrap_or(&cmd).to_string();
                                        state.allowed_safe_patterns.insert(prefix.clone());
                                        state.history.push(Cell {
                                            kind: CellKind::Log,
                                            payload: format!("Whitelist updated: '{prefix}' will auto-execute for this session."),
                                        });
                                        state.history.push(Cell {
                                            kind: CellKind::User,
                                            payload: format!("!{cmd}"),
                                        });
                                        state.active_action = Some(format!("$ {cmd}"));
                                        execute_shell_command(cmd, tx.clone());
                                    }
                                    _ => {
                                        // Reject
                                        state.approval_state = crate::tui::state::ApprovalState::None;
                                        state.history.push(Cell {
                                            kind: CellKind::Log,
                                            payload: format!("Command cancelled: {cmd}"),
                                        });
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
                TuiMessage::TurnComplete(success) => {
                    // Finish the stream controller and drain all remaining lines
                    state.stream_controller.finish();
                    let remaining = state.stream_controller.drain_all();
                    state.history.extend(remaining);

                    // Also flush legacy stream state as a safety net
                    let renderer = crate::render::TerminalRenderer::new();
                    if let Some(flushed) = state.stream_state.flush(&renderer) {
                        for line in flushed.lines() {
                            state.history.push(Cell {
                                kind: CellKind::AgentMessage,
                                payload: line.to_string(),
                            });
                        }
                    }
                    state.active_action = None;
                    state.task_start_time = None;
                    // Reset streaming metrics (Yomi pattern)
                    state.is_streaming = false;
                    state.streaming_token_estimate = 0.0;
                    state.streaming_start_time = None;
                    let was_cancelled = state
                        .cancellation
                        .as_ref()
                        .is_some_and(state::CancellationToken::is_cancelled);
                    state.cancellation = None;

                    if success && !was_cancelled {
                        state.history.push(Cell {
                            kind: CellKind::Result,
                            payload: "Done".into(),
                        });

                        if let Some(next_task) = state.task_queue.pop_front() {
                            crate::tui::commands::execute_command_string(state, next_task, tx, &cfg, thread_manager);
                        }
                    } else if !state.task_queue.is_empty() {
                        let drop_count = state.task_queue.len();
                        state.task_queue.clear();
                        state.history.push(Cell {
                            kind: CellKind::Error,
                            payload: format!(
                                "Pipeline halted. Dropped {drop_count} queued queries."
                            ),
                        });
                    }

                    // Refresh git state post-turn in case files were modified
                    crate::tui::app::refresh_git_state(state, &cfg.workspace);
                }
                TuiMessage::SessionComplete => {
                    // Drain any remaining stream content on session end
                    state.stream_controller.finish();
                    let remaining = state.stream_controller.drain_all();
                    state.history.extend(remaining);

                    state.active_action = None;
                    state.task_start_time = None;
                    state.is_streaming = false;
                    state.streaming_token_estimate = 0.0;
                    state.streaming_start_time = None;
                    state.cancellation = None;
                    crate::tui::app::refresh_git_state(state, &cfg.workspace);
                }
                TuiMessage::SwarmStarted(id, task) => {
                    state.active_swarms.push((id, task));
                }
                TuiMessage::SwarmComplete(id, success) => {
                    state
                        .active_swarms
                        .retain(|(active_id, _)| active_id != &id);
                    state.history.push(Cell {
                        kind: if success {
                            CellKind::Result
                        } else {
                            CellKind::Error
                        },
                        payload: format!("Swarm worker [{id}] finished."),
                    });
                }
                TuiMessage::Tick => {
                    state.spinner_idx = state.spinner_idx.wrapping_add(1);

                    // ── CommitTick: drain buffered stream lines ──
                    let drained = state.stream_controller.drain_tick();
                    if !drained.is_empty() {
                        state.history.extend(drained);
                        // Auto-scroll to bottom when new streaming content arrives
                        state.scroll_offset = 0;
                    }

                    // Auto-clear expired status messages (Yomi pattern)
                    state.check_status_timeout();

                    if let Some(start) = state.task_start_time {
                        if start.elapsed() > Duration::from_secs(180) {
                            state.history.push(Cell {
                                kind: CellKind::Error,
                                payload: "Network response or task execution is taking over 3 minutes. Is it hanging? Press ESC to force-interrupt.".into(),
                            });
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
                TuiMessage::Quit => break,
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
                        state.history.push(Cell {
                            kind: CellKind::Log,
                            payload: format!("Turn started: {turn_id}"),
                        });
                    }
                }
                TurnEvent::Completed {
                    turn_id, success, ..
                } => {
                    if state.view_mode == ViewMode::Audit {
                        let status = if success { "✓" } else { "✘" };
                        state.history.push(Cell {
                            kind: CellKind::Log,
                            payload: format!("{status} Turn completed: {turn_id}"),
                        });
                    }
                }
                TurnEvent::Aborted { turn_id, reason } => {
                    state.history.push(Cell {
                        kind: CellKind::Error,
                        payload: format!("Turn aborted [{turn_id}]: {reason}"),
                    });
                }
                TurnEvent::PhaseChanged { phase, .. } => {
                    state.active_action = Some(format!("{phase}"));
                }
            }
        }
        AgentEvent::Thinking(_, _) => {
            state.active_action = Some("Thinking...".into());
            // Start a fresh streaming session for this turn
            state.stream_controller.start();
            // Start streaming metrics (Yomi InfoBar pattern)
            state.is_streaming = true;
            state.streaming_token_estimate = 0.0;
            state.streaming_start_time = Some(Instant::now());
        }
        AgentEvent::StreamChunk(chunk) => {
            // CommitTick pattern: buffer chunks into the stream controller.
            // Lines are drained one-per-tick in the Tick handler for smooth animation.
            state.stream_controller.push_chunk(&chunk);
            // Accumulate token estimate for InfoBar display
            state.streaming_token_estimate += AppState::estimate_tokens(&chunk);
        }
        AgentEvent::Markdown(md) => {
            // Final markdown block: route through controller for buffered drain.
            state.stream_controller.push_markdown(&md);
        }
        AgentEvent::Log(msg) => {
            state.history.push(Cell {
                kind: CellKind::Log,
                payload: msg,
            });
        }
        AgentEvent::ActionStart(desc) => {
            state.active_action = Some(desc);
        }
        AgentEvent::ActionComplete(desc) => {
            state.history.push(Cell {
                kind: CellKind::Action,
                payload: desc,
            });
        }
        AgentEvent::ReadFiles(paths) => {
            if state.view_mode != ViewMode::Focus {
                let display = if paths.len() <= 3 {
                    paths.join(", ")
                } else {
                    format!("{}, ... ({} files)", paths[..2].join(", "), paths.len())
                };
                state.history.push(Cell {
                    kind: CellKind::Evidence,
                    payload: format!("Read {display}"),
                });
            }
        }
        AgentEvent::ReconStart(desc) => {
            state.active_action = Some(format!("Recon: {desc}"));
        }
        AgentEvent::DelegateStart(id, task) => {
            state.active_action = Some(format!("Delegating: {task}"));
            state.active_swarms.push((id, task));
        }
        AgentEvent::DelegateComplete(id, _success) => {
            state.active_swarms.retain(|(active_id, _)| active_id != &id);
        }
        AgentEvent::PlanSubmitted(plan) => {
            if !plan.operations.is_empty() {
                state.history.push(Cell {
                    kind: CellKind::Action,
                    payload: format!("{} operations planned", plan.operations.len()),
                });
            }
        }
        AgentEvent::CruciblePreflight(msg) => {
            state.active_action = Some(format!("Verifying: {msg}"));
        }
        AgentEvent::Error(err) => {
            state.history.push(Cell {
                kind: CellKind::Error,
                payload: err,
            });
            state.active_action = None;
            state.task_start_time = None;
        }
        // ── High-granularity events (Yomi-inspired) ─────────────────────
        AgentEvent::TokenUsage {
            total_tokens,
            context_window,
            ..
        } => {
            // Update context window usage for status bar (Yomi pattern)
            state.ctx_usage = Some((total_tokens, context_window));
        }
        AgentEvent::StateChanged { from, to } => {
            if state.view_mode == ViewMode::Audit {
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: format!("State: {from} → {to}"),
                });
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
                state::StatusMessage::warn(format!("Retrying ({attempt}/{max_attempts}): {reason}")),
                5000,
            );
        }
        AgentEvent::Compacting { active } => {
            if active {
                state.active_action = Some("Compacting context…".into());
            } else {
                state.active_action = None;
                state.history.push(Cell {
                    kind: CellKind::Action,
                    payload: "Context compaction complete".into(),
                });
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
