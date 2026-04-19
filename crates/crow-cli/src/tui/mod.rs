pub mod components;
pub mod dashboard;
pub mod render;
pub mod state;

pub use dashboard::run_dashboard;

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

use crate::context::ConversationManager;
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
        if let Ok(store) = crate::session::SessionStore::open() {
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

    let thread_manager = Arc::new(crate::thread_manager::ThreadManager::new(
        shared_runtime.clone(),
        shared_messages.clone(),
        cfg_val.clone(),
        tx.clone(),
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
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
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

fn handle_tab_completion(state: &mut AppState) {
    let text = state.composer.clone();

    // Universal file path completion for the last token in the composer.
    let last_word_idx = text
        .rfind(|c: char| c.is_whitespace())
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let path_prefix = &text[last_word_idx..];

    if !path_prefix.is_empty() {
        let mut parent_dir = std::path::Path::new(".");
        let mut file_prefix = path_prefix;

        if let Some(slash_idx) = path_prefix.rfind('/') {
            parent_dir = std::path::Path::new(&path_prefix[..=slash_idx]);
            file_prefix = &path_prefix[slash_idx + 1..];
            if parent_dir.as_os_str().is_empty() {
                parent_dir = std::path::Path::new("/");
            }
        }

        if let Ok(entries) = std::fs::read_dir(parent_dir) {
            let mut matches = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(file_prefix) {
                    if entry.path().is_dir() {
                        matches.push(format!("{name}/"));
                    } else {
                        matches.push(name);
                    }
                }
            }

            if matches.len() == 1 {
                let parent_str = parent_dir.to_string_lossy();
                let parent_part = if parent_str == "." || parent_str == "./" {
                    String::new()
                } else {
                    parent_str.to_string()
                };

                let new_text = format!("{}{}{}", &text[..last_word_idx], parent_part, matches[0]);

                state.composer = new_text;
                state.composer_cursor = state.composer.chars().count();
                return;
            }
        }
    }

    // Normal text mode fallback: insert 4 spaces if no completion match
    for _ in 0..4 {
        insert_char_at_cursor(state, ' ');
    }
}

fn insert_char_at_cursor(state: &mut AppState, c: char) {
    let mut chars: Vec<char> = state.composer.chars().collect();
    // Clamp cursor, just in case
    let cursor = state.composer_cursor.min(chars.len());
    chars.insert(cursor, c);
    state.composer = chars.into_iter().collect();
    state.composer_cursor += 1;
}

async fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    rx: &mut mpsc::UnboundedReceiver<TuiMessage>,
    tx: &mpsc::UnboundedSender<TuiMessage>,
    cfg: CrowConfig,
    thread_manager: &Arc<crate::thread_manager::ThreadManager>,
) -> Result<()> {
    loop {
        terminal.draw(|f| render_app(f, state))?;

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
                    if let crate::tui::state::ApprovalState::PendingCommand(cmd) =
                        state.approval_state.clone()
                    {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                                // Approved
                                state.approval_state = crate::tui::state::ApprovalState::None;
                                state.history.push(Cell {
                                    kind: CellKind::User,
                                    payload: format!("!{cmd}"),
                                });
                                state.active_action = Some(format!("$ {cmd}"));
                                execute_shell_command(cmd, tx.clone());
                            }
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                // Approved Always
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
                                // Cancelled
                                state.approval_state = crate::tui::state::ApprovalState::None;
                                state.history.push(Cell {
                                    kind: CellKind::Log,
                                    payload: format!("Command cancelled: {cmd}"),
                                });
                            }
                        }
                        continue;
                    }

                    // ── Overlay Modal Interception ──────────────────────────────
                    if let crate::tui::state::OverlayState::CommandPalette {
                        query: _,
                        selected_idx,
                    } = &mut state.overlay_state
                    {
                        match key.code {
                            KeyCode::Esc => {
                                state.overlay_state = crate::tui::state::OverlayState::None;
                            }
                            KeyCode::Down => {
                                let commands =
                                    crate::tui::state::get_palette_commands(&state.composer);
                                let max_idx = commands.len().saturating_sub(1);
                                *selected_idx = (*selected_idx + 1).min(max_idx);
                            }
                            KeyCode::Up => {
                                *selected_idx = selected_idx.saturating_sub(1);
                            }
                            KeyCode::Enter => {
                                let commands =
                                    crate::tui::state::get_palette_commands(&state.composer);
                                let cmd = if let Some((c, _)) = commands.get(*selected_idx) {
                                    c.clone()
                                } else {
                                    String::new()
                                };
                                let cmd_string = cmd;
                                state.overlay_state = crate::tui::state::OverlayState::None;
                                state.composer.clear();
                                state.composer_cursor = 0;
                                // Need to manually execute command
                                execute_command_string(state, cmd_string, tx, &cfg, thread_manager);
                            }
                            KeyCode::Backspace => {
                                // If they backspace past 0, close overlay. Else pop char from composer.
                                if state.composer.is_empty() {
                                    state.overlay_state = crate::tui::state::OverlayState::None;
                                } else {
                                    state.composer.pop();
                                    state.composer_cursor = state.composer.chars().count();
                                    if state.composer.is_empty() {
                                        state.overlay_state = crate::tui::state::OverlayState::None;
                                    } else if let crate::tui::state::OverlayState::CommandPalette { query, selected_idx } = &mut state.overlay_state {
                                        *query = state.composer.clone();
                                        *selected_idx = 0; // reset selection
                                    }
                                }
                            }
                            KeyCode::Char(c) => {
                                // Let normal typing carry into composer
                                insert_char_at_cursor(state, c);
                                if let crate::tui::state::OverlayState::CommandPalette {
                                    query,
                                    selected_idx,
                                } = &mut state.overlay_state
                                {
                                    *query = state.composer.clone();
                                    *selected_idx = 0; // reset selection
                                }
                            }
                            KeyCode::Tab => {
                                handle_tab_completion(state);
                                if let crate::tui::state::OverlayState::CommandPalette {
                                    query,
                                    selected_idx,
                                } = &mut state.overlay_state
                                {
                                    *query = state.composer.clone();
                                    *selected_idx = 0; // reset selection
                                }
                            }
                            _ => {}
                        }
                        continue; // Do not pass modal keys through
                    }

                    match key.code {
                        // ── Ctrl+J: newline in composer ──────────────────────
                        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            insert_char_at_cursor(state, '\n');
                        }

                        // ── Ctrl+L: clear screen ─────────────────────────────
                        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.history.clear();
                            state.scroll_offset = 0;
                        }

                        // ── Ctrl+U: clear composer line ──────────────────────
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.composer.clear();
                            state.composer_cursor = 0;
                        }

                        // ── Enter: submit ────────────────────────────────────
                        KeyCode::Enter => {
                            handle_enter(state, tx, &cfg, thread_manager);
                        }

                        // ── Up/Down: input history ───────────────────────────
                        KeyCode::Up if !state.input_history.is_empty() => {
                            let idx = match state.input_history_idx {
                                Some(i) => i.saturating_sub(1),
                                None => state.input_history.len() - 1,
                            };
                            state.input_history_idx = Some(idx);
                            state.composer = state.input_history[idx].clone();
                            state.composer_cursor = state.composer.chars().count();
                        }
                        KeyCode::Down => {
                            if let Some(idx) = state.input_history_idx {
                                let next = idx + 1;
                                if next >= state.input_history.len() {
                                    state.input_history_idx = None;
                                    state.composer.clear();
                                    state.composer_cursor = 0;
                                } else {
                                    state.input_history_idx = Some(next);
                                    state.composer = state.input_history[next].clone();
                                    state.composer_cursor = state.composer.chars().count();
                                }
                            }
                        }

                        // ── Left/Right: cursor movement ──────────────────────
                        KeyCode::Left => {
                            state.composer_cursor = state.composer_cursor.saturating_sub(1);
                        }
                        KeyCode::Right => {
                            let len = state.composer.chars().count();
                            if state.composer_cursor < len {
                                state.composer_cursor += 1;
                            }
                        }

                        // ── Text input ───────────────────────────────────────
                        KeyCode::Char(c) => {
                            insert_char_at_cursor(state, c);
                            if state.composer.starts_with('/') {
                                state.overlay_state =
                                    crate::tui::state::OverlayState::CommandPalette {
                                        query: state.composer.clone(),
                                        selected_idx: 0,
                                    };
                            }
                        }
                        KeyCode::Backspace if state.composer_cursor > 0 => {
                            let idx = state.composer_cursor - 1;
                            let mut chars: Vec<char> = state.composer.chars().collect();
                            chars.remove(idx);
                            state.composer = chars.into_iter().collect();
                            state.composer_cursor -= 1;
                        }
                        KeyCode::Delete => {
                            let chars: Vec<char> = state.composer.chars().collect();
                            if state.composer_cursor < chars.len() {
                                let mut new_chars = chars;
                                new_chars.remove(state.composer_cursor);
                                state.composer = new_chars.into_iter().collect();
                            }
                        }
                        KeyCode::Tab => {
                            handle_tab_completion(state);
                        }
                        _ => {}
                    }
                }
                Event::Paste(text) => {
                    for c in text.chars() {
                        if c != '\r' {
                            insert_char_at_cursor(state, c);
                        }
                    }
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
                    // Flush any remaining stream buffer
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
                    state.active_action = None;
                    state.task_start_time = None;
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
                        // ~ once per second (120ms * 8)tick)
                        let draft_path =
                            std::path::Path::new(&cfg.workspace).join(".crow/logs/draft.txt");
                        if !state.composer.is_empty() {
                            let _ = std::fs::write(&draft_path, &state.composer);
                        } else {
                            let _ = std::fs::remove_file(&draft_path);
                        }
                    }
                }
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
                // Signal quit by pushing a sentinel (handled in main loop)
                // For now, we use a clean exit mechanism through the caller.
                // The user can also use Ctrl+D to quit instantly.
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: "Use Ctrl+D to exit, or Ctrl+C twice.".into(),
                });
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
                    match crate::session::SessionStore::open() {
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

        let is_safe = safe_prefixes
            .iter()
            .any(|safe| bash_cmd == *safe || bash_cmd.starts_with(&format!("{safe} ")))
            || state
                .allowed_safe_patterns
                .iter()
                .any(|safe| bash_cmd == *safe || bash_cmd.starts_with(&format!("{safe} ")));

        if is_safe {
            state.history.push(Cell {
                kind: CellKind::User,
                payload: format!("!{bash_cmd}"),
            });
            execute_shell_command(bash_cmd, tx.clone());
        } else {
            state.approval_state = crate::tui::state::ApprovalState::PendingCommand(bash_cmd);
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
        AgentEvent::Thinking(_, _) => {
            state.active_action = Some("Thinking...".into());
        }
        AgentEvent::StreamChunk(chunk) => {
            // Incremental markdown streaming: accumulate chunks and flush
            // rendered ANSI at safe paragraph/fence boundaries.
            let renderer = crate::render::TerminalRenderer::new();
            if let Some(rendered) = state.stream_state.push(&renderer, &chunk) {
                // A safe boundary was found — push rendered markdown to history
                for line in rendered.lines() {
                    state.history.push(Cell {
                        kind: CellKind::AgentMessage,
                        payload: line.to_string(),
                    });
                }
            }
        }
        AgentEvent::Markdown(md) => {
            // Flush any remaining stream buffer before rendering final markdown
            let renderer = crate::render::TerminalRenderer::new();
            if let Some(flushed) = state.stream_state.flush(&renderer) {
                for line in flushed.lines() {
                    state.history.push(Cell {
                        kind: CellKind::AgentMessage,
                        payload: line.to_string(),
                    });
                }
            }
            // Then render the final markdown block
            let rendered = renderer.render_markdown(&md);
            for line in rendered.lines() {
                state.history.push(Cell {
                    kind: CellKind::AgentMessage,
                    payload: line.to_string(),
                });
            }
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
            prompt_tokens,
            completion_tokens,
            total_tokens,
            context_window,
        } => {
            // Store on AppState for info bar rendering
            let pct = (total_tokens * 100)
                .checked_div(context_window)
                .unwrap_or(0);
            state.model_info = format!(
                "{} | Tokens: {}+{}={} ({}%)",
                state
                    .model_info
                    .split(" | Tokens:")
                    .next()
                    .unwrap_or(&state.model_info),
                prompt_tokens,
                completion_tokens,
                total_tokens,
                pct
            );
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
            state.history.push(Cell {
                kind: CellKind::Log,
                payload: format!("⚠ Retrying ({attempt}/{max_attempts}): {reason}"),
            });
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
