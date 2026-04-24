use crate::config::CrowConfig;
use anyhow::Result;
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use crate::tui::state::{AppState, Cell, CellKind, TuiMessage};
use crate::tui::event_loop::run_tui_loop;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use std::io;

use crow_runtime::context::ConversationManager;
use crate::runtime::SessionRuntime;

use crate::tui::theme;

/// Duration within which a second Ctrl+C quits (Codex pattern).
pub fn refresh_git_state(state: &mut AppState, workspace: &std::path::Path) {
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
                crow_runtime::event::EngineEvent::TurnComplete(b, timing) => { let _ = tx_for_engine.send(TuiMessage::TurnComplete(b, timing)); }
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

    // Clear terminal title before leaving alternate screen
    let _ = crate::tui::terminal_title::clear_terminal_title();

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
