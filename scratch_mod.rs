pub mod component;
pub mod components;

pub mod markdown_stream;
pub mod render;
pub mod state;
pub mod stream_controller;
pub mod theme;



use crate::config::CrowConfig;
use crate::event::{AgentEvent, ViewMode};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use render::render_app;
use state::{AppState, Cell, CellKind, TuiMessage};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};

use crow_runtime::context::ConversationManager;
use crate::runtime::SessionRuntime;

/// Duration within which a second Ctrl+C quits (Codex pattern).
const CTRL_C_QUIT_WINDOW: Duration = Duration::from_millis(1500);

fn refresh_git_state(state: &mut AppState, workspace: &std::path::Path) {
    if let Ok(output) = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(workspace)
        .output()
    {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !branch.is_empty() {
            state.git_branch = branch;
        }
    }
    if let Ok(output) = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
    {
        state.is_dirty = !output.stdout.is_empty();
    }
}

pub async fn run_workbench(cfg_val: &CrowConfig, resume: bool) -> Result<()> {
    // Auto-detect terminal background (light/dark) and set theme (codex pattern).
    // Must happen before entering alternate screen which may change terminal state.
    theme::init_theme();

    // Install a panic hook BEFORE entering alternate screen so that if any
    // code panics during the TUI loop the terminal is left in a sane state
    // instead of raw mode with the alternate screen still active.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste);
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Derive workspace name from config path
    let workspace_name = std::path::Path::new(&cfg_val.workspace)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut state = AppState::new(
        cfg_val.llm.model.clone(),
        format!("{}", cfg_val.write_mode),
        workspace_name,
    );

    // Initial git state fetch
    refresh_git_state(&mut state, &cfg_val.workspace);

    // Try to Resume
    let mut loaded_messages = vec![];
    let mut loaded_session_id = None;
    if resume {
        if let Ok(store) = crow_runtime::session::SessionStore::open() {
            if let Ok(Some(session)) = store.find_latest_for_workspace(&cfg_val.workspace) {
                loaded_messages = session.restore_messages();
                loaded_session_id = Some(session.id.0.clone());
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: format!(
                        "Resumed session {} ({} messages) from Task: {}",
                        session.id.0,
                        loaded_messages.len(),
                        session.task
                    ),
                });
                for msg in &loaded_messages {
                    let kind = match msg.role {
                        crow_brain::ChatRole::User => CellKind::User,
                        _ => CellKind::AgentMessage,
                    };
                    state.history.push(Cell {
                        kind,
                        payload: msg.content.clone(),
                    });
                }
            } else {
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: "No previous session found for this workspace to resume.".into(),
                });
            }
        }
    }

    // Initial draft recovery
    let _ = std::fs::create_dir_all(std::path::Path::new(&cfg_val.workspace).join(".crow/logs"));
    let draft_path = std::path::Path::new(&cfg_val.workspace).join(".crow/logs/draft.txt");
    if let Ok(draft_content) = std::fs::read_to_string(&draft_path) {
        if !draft_content.trim().is_empty() {
            state.composer = draft_content;
            state.composer_cursor = state.composer.chars().count();
        }
    }

    let messages = ConversationManager::new(loaded_messages);
    let runtime = SessionRuntime::boot(cfg_val).await?;

    let shared_runtime = Arc::new(runtime);
    let shared_messages = Arc::new(Mutex::new(messages));

    let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
    let tx_for_engine = tx.clone();
    tokio::spawn(async move {
        while let Some(evt) = engine_rx.recv().await {
            match evt {
                crow_runtime::event::EngineEvent::AgentEvent(e) => { let _ = tx_for_engine.send(TuiMessage::AgentEvent(e)); }
                crow_runtime::event::EngineEvent::SessionComplete => { let _ = tx_for_engine.send(TuiMessage::SessionComplete); }
                crow_runtime::event::EngineEvent::TurnComplete(b) => { let _ = tx_for_engine.send(TuiMessage::TurnComplete(b)); }
                crow_runtime::event::EngineEvent::SwarmStarted(a, b) => { let _ = tx_for_engine.send(TuiMessage::SwarmStarted(a, b)); }
                crow_runtime::event::EngineEvent::SwarmComplete(a, b) => { let _ = tx_for_engine.send(TuiMessage::SwarmComplete(a, b)); }
            }
        }
    });

    let thread_manager = Arc::new(crate::thread_manager::ThreadManager::new(
        shared_runtime.clone(),
        shared_messages.clone(),
        cfg_val.clone(),
        engine_tx,
        loaded_session_id,
    ));

    // Tick timer for spinner animation
    let tx_tick = tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(120)).await;
            if tx_tick.send(TuiMessage::Tick).is_err() {
                break;
            }
        }
    });

    let res = run_tui_loop(
        &mut terminal,
        &mut state,
        &mut rx,
        &tx,
        cfg_val.clone(),
        &thread_manager,
    )
    .await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("Crow: {err:?}");
    }
    Ok(())
}

fn execute_shell_command(bash_cmd: String, tx: mpsc::UnboundedSender<TuiMessage>) {
    tokio::spawn(async move {
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&bash_cmd)
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout_stripped = strip_ansi_escapes::strip(&out.stdout);
                let stderr_stripped = strip_ansi_escapes::strip(&out.stderr);
                let stdout = String::from_utf8_lossy(&stdout_stripped).into_owned();
                let stderr = String::from_utf8_lossy(&stderr_stripped).into_owned();
                let mut report = stdout;
                if !stderr.is_empty() {
                    if !report.is_empty() {
                        report.push('\n');
                    }
                    report.push_str(&stderr);
                }
                if report.trim().is_empty() {
                    report = "(no output)".into();
                }
                let _ = tx.send(TuiMessage::AgentEvent(AgentEvent::Log(report)));
            }
            Err(e) => {
                let _ = tx.send(TuiMessage::AgentEvent(AgentEvent::Error(format!(
                    "Failed: {e}"
                ))));
            }
        }
        let _ = tx.send(TuiMessage::SessionComplete);
    });
}

async fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    rx: &mut mpsc::UnboundedReceiver<TuiMessage>,
    tx: &mpsc::UnboundedSender<TuiMessage>,
    cfg: CrowConfig,
    thread_manager: &Arc<crate::thread_manager::ThreadManager>,
) -> Result<()> {
    let mut composer_comp = components::composer::ComposerComponent::new();
    let mut history_comp = components::history::HistoryComponent::new();

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
                            execute_command_string(state, next_task, tx, &cfg, thread_manager);
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
                    refresh_git_state(state, &cfg.workspace);
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
                    refresh_git_state(state, &cfg.workspace);
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

// ── Enter handler ────────────────────────────────────────────────────────────

fn handle_enter(
    state: &mut AppState,
    tx: &mpsc::UnboundedSender<TuiMessage>,
    cfg: &CrowConfig,
    thread_manager: &Arc<crate::thread_manager::ThreadManager>,
) {
    let prompt = state.composer.clone();
    if prompt.trim().is_empty() {
        return;
    }

    let trimmed = prompt.trim();

    // Save to input history (skip slash commands)
    if !trimmed.starts_with('/') {
        state.input_history.push(prompt.clone());
    }
    state.input_history_idx = None;
    state.scroll_offset = 0;

    execute_command_string(state, prompt, tx, cfg, thread_manager);
}

fn execute_command_string(
    state: &mut AppState,
    prompt: String,
    tx: &mpsc::UnboundedSender<TuiMessage>,
    _cfg: &CrowConfig,
    thread_manager: &Arc<crate::thread_manager::ThreadManager>,
) {
    let trimmed = prompt.trim();

    // ── Slash commands ───────────────────────────────────────────────
    if trimmed.starts_with('/') {
        let mut parts = trimmed.trim_start_matches('/').split_whitespace();
        let cmd = parts.next().unwrap_or_default();
        match cmd {
            "exit" | "quit" | "q" => {
                state.composer.clear();
                state.composer_cursor = 0;
                let _ = tx.send(TuiMessage::Quit);
            }
            "clear" | "c" => {
                state.history.clear();
                let tm = thread_manager.clone();
                tokio::spawn(async move {
                    tm.submit(crate::thread_manager::Op::Clear).await;
                });
            }
            "swarm" => {
                let payload = parts.collect::<Vec<_>>().join(" ");
                if payload.is_empty() {
                    state.history.push(Cell {
                        kind: CellKind::Error,
                        payload: "Usage: /swarm <task description>".into(),
                    });
                } else {
                    let tm = thread_manager.clone();
                    tokio::spawn(async move {
                        tm.submit(crate::thread_manager::Op::SwarmRun(payload))
                            .await;
                    });
                    state.history.push(Cell {
                        kind: CellKind::Log,
                        payload: "Launched asynchronous Sub-Agent Swarm Worker.".into(),
                    });
                }
            }
            "help" | "?" => {
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: "/help".into(),
                });
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: [
                        "Commands:",
                        "  /help          Show this message",
                        "  /status        Workspace health",
                        "  /clear         Clear conversation",
                        "  /view <mode>   Set view (focus|evidence|audit)",
                        "  /model         Show current model",
                        "  /swarm <task>  Launch background sub-agent",
                        "  /compact       Force context compaction",
                        "",
                        "Shortcuts:",
                        "  Ctrl+C         Interrupt / quit (press twice)",
                        "  Ctrl+D         Quit immediately",
                        "  Ctrl+J         Insert newline",
                        "  Ctrl+L         Clear screen",
                        "  Ctrl+U         Clear input",
                        "  Esc            Interrupt running task",
                        "  !<cmd>         Execute shell command",
                    ]
                    .join("\n"),
                });
            }
            "status" => {
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: "/status".into(),
                });
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: format!(
                        "Model: {}\nWorkspace: {}\nWrite Mode: {}\nView: {:?}",
                        state.model_info, state.workspace_name, state.write_mode, state.view_mode,
                    ),
                });
            }
            "model" => {
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: "/model".into(),
                });
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: format!("Current model: {}", state.model_info),
                });
            }
            "view" => {
                let mode = parts.next().unwrap_or("evidence");
                state.view_mode = match mode {
                    "focus" => ViewMode::Focus,
                    "audit" => ViewMode::Audit,
                    _ => ViewMode::Evidence,
                };
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: format!("View mode: {:?}", state.view_mode),
                });
            }
            "compact" => {
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: "/compact".into(),
                });
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: "Context compaction will run before the next turn.".into(),
                });
            }
            "session" => {
                let action = parts.next().unwrap_or("list");
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: format!("/session {action}"),
                });

                if action == "list" {
                    match crow_runtime::session::SessionStore::open() {
                        Ok(store) => match store.list() {
                            Ok(summaries) => {
                                let mut out = String::from("Saved sessions:\n");
                                for summary in summaries.into_iter().take(10) {
                                    out.push_str(&format!("{summary}\n"));
                                }
                                state.history.push(Cell {
                                    kind: CellKind::Log,
                                    payload: out,
                                });
                            }
                            Err(e) => {
                                state.history.push(Cell {
                                    kind: CellKind::Error,
                                    payload: format!("Failed to list sessions: {e}"),
                                });
                            }
                        },
                        Err(e) => {
                            state.history.push(Cell {
                                kind: CellKind::Error,
                                payload: format!("Failed to open session store: {e}"),
                            });
                        }
                    }
                } else if action == "resume" {
                    let maybe_id = parts.next();
                    if let Some(_id) = maybe_id {
                        state.history.push(Cell {
                            kind: CellKind::Log,
                            payload: "To resume a session, restart crow using: crow -r <id>".into(),
                        });
                    } else {
                        state.history.push(Cell {
                            kind: CellKind::Error,
                            payload: "Usage: /session resume <id>".into(),
                        });
                    }
                }
            }
            other => {
                state.history.push(Cell {
                    kind: CellKind::Error,
                    payload: format!(
                        "Unknown command: /{other}. Type /help for available commands."
                    ),
                });
            }
        }
        state.composer.clear();
        state.composer_cursor = 0;
        return;
    }

    // ── Pre-execution Queue Check ────────────────────────────────────
    if state.is_task_running() {
        state.task_queue.push_back(prompt.clone());
        state.history.push(Cell {
            kind: CellKind::User,
            payload: prompt.clone(),
        });
        state.history.push(Cell {
            kind: CellKind::Log,
            payload: "Queued for execution...".into(),
        });
        state.composer.clear();
        state.composer_cursor = 0;
        return;
    }

    // ── Shell commands (!cmd) ────────────────────────────────────────
    if trimmed.starts_with('!') {
        let bash_cmd = trimmed.trim_start_matches('!').trim().to_string();

        let safe_prefixes = [
            "ls",
            "pwd",
            "echo",
            "cat",
            "git status",
            "git branch",
            "git diff",
            "git log",
            "git show",
            "whoami",
            "date",
            "tree",
            "hostname",
            "cargo check",
            "cargo build",
            "cargo test",
        ];

        // SECURITY: Reject commands with shell metacharacters from the fast
        // path. Execution goes through `sh -c`, so `!cargo test && curl ...`
        // would bypass the prefix allowlist without this check.
        const SHELL_METACHARACTERS: &[&str] = &[
            "&&", "||", ";", "|", "$(", "${", "$", "`", ">", "<", "(", ")", "{", "}", "\n", "\\", "#",
        ];
        let has_metacharacters = SHELL_METACHARACTERS
            .iter()
            .any(|meta| bash_cmd.contains(meta));

        let prefix_matches = safe_prefixes
            .iter()
            .any(|safe| bash_cmd == *safe || bash_cmd.starts_with(&format!("{safe} ")))
            || state
                .allowed_safe_patterns
                .iter()
                .any(|safe| bash_cmd == *safe || bash_cmd.starts_with(&format!("{safe} ")));

        let is_safe = prefix_matches && !has_metacharacters;

        if is_safe {
            state.history.push(Cell {
                kind: CellKind::User,
                payload: format!("!{bash_cmd}"),
            });
            execute_shell_command(bash_cmd, tx.clone());
        } else {
            state.approval_state = crate::tui::state::ApprovalState::PendingCommand(bash_cmd, 0);
        }

        state.composer.clear();
        state.composer_cursor = 0;
        return;
    }

    // ── Normal prompt: send to agent ─────────────────────────────────
    state.history.push(Cell {
        kind: CellKind::User,
        payload: prompt.clone(),
    });

    state.active_action = Some("Thinking...".into());
    state.task_start_time = Some(Instant::now());

    let tm = thread_manager.clone();
    tokio::spawn(async move {
        tm.submit(crate::thread_manager::Op::Input(prompt)).await;
    });

    state.composer.clear();
    state.composer_cursor = 0;
}

// ── Agent event handler ──────────────────────────────────────────────────────

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
        AgentEvent::DelegateStart(task) => {
            state.active_action = Some(format!("Delegating: {task}"));
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
