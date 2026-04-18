use crate::config::CrowConfig;
use crate::render::{ColorTheme, TerminalRenderer};
use crate::session::{Session, SessionStore};
use anyhow::Result;
use crossterm::style::{Color, Stylize};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Cmd, CompletionType, Config, Context, EditMode, Editor, Helper, KeyCode, KeyEvent, Modifiers};
use std::borrow::Cow;
use std::cell::RefCell;
use std::time::Instant;

// ─── Slash Commands ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum SlashCommand {
    Help,
    Status,
    Clear,
    Compact,
    Exit,
    Unknown(String),
}

impl SlashCommand {
    fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let command = trimmed
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default();
        Some(match command {
            "help" | "?" => Self::Help,
            "status" => Self::Status,
            "clear" => Self::Clear,
            "compact" => Self::Compact,
            "exit" | "quit" => Self::Exit,
            other => Self::Unknown(other.to_string()),
        })
    }

    fn all_names() -> Vec<String> {
        vec![
            "/help".into(),
            "/status".into(),
            "/clear".into(),
            "/compact".into(),
            "/exit".into(),
        ]
    }
}

// ─── Session State ──────────────────────────────────────────────────

struct SessionState {
    turns: usize,
    total_duration_ms: u128,
}

impl SessionState {
    fn new() -> Self {
        Self {
            turns: 0,
            total_duration_ms: 0,
        }
    }
}

// ─── Slash Command Helper (Tab-completion) ──────────────────────────

struct CrowHelper {
    completions: Vec<String>,
    current_line: RefCell<String>,
}

impl CrowHelper {
    fn new(completions: Vec<String>) -> Self {
        Self {
            completions,
            current_line: RefCell::new(String::new()),
        }
    }
}

impl Completer for CrowHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        if pos != line.len() || line.contains(char::is_whitespace) || !line.starts_with('/') {
            return Ok((0, Vec::new()));
        }

        let matches = self
            .completions
            .iter()
            .filter(|c| c.starts_with(line))
            .map(|c| Pair {
                display: c.clone(),
                replacement: c.clone(),
            })
            .collect();

        Ok((0, matches))
    }
}

impl Hinter for CrowHelper {
    type Hint = String;
}

impl Highlighter for CrowHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        *self.current_line.borrow_mut() = line.to_string();
        Cow::Borrowed(line)
    }
}

impl Validator for CrowHelper {}
impl Helper for CrowHelper {}

// ─── REPL ───────────────────────────────────────────────────────────

/// Enter continuous chat REPL mode with rich terminal UX.
pub async fn run_repl(cfg: &CrowConfig) -> Result<()> {
    let renderer = TerminalRenderer::new();
    let theme = *renderer.color_theme();

    // ── Welcome Banner ──
    println!();
    println!(
        "{}",
        "  🦅 Crow Code — Evidence-Driven Coding Agent"
            .bold()
            .with(Color::Cyan)
    );
    println!(
        "  {}",
        format!(
            "Model: {} │ Write: {:?}",
            cfg.describe_provider(),
            cfg.write_mode
        )
        .with(Color::DarkGrey)
    );
    println!(
        "  {}",
        "Type /help for commands. Ctrl+J or Shift+Enter for newlines."
            .with(Color::DarkGrey)
    );
    println!(
        "  {}",
        "───────────────────────────────────────────────"
            .with(Color::DarkGrey)
    );
    println!();

    // ── Editor Setup ──
    let editor_config = Config::builder()
        .completion_type(CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .build();
    let mut rl =
        Editor::<CrowHelper, DefaultHistory>::with_config(editor_config)?;
    rl.set_helper(Some(CrowHelper::new(SlashCommand::all_names())));
    // Ctrl+J inserts a newline (multi-line input)
    rl.bind_sequence(KeyEvent(KeyCode::Char('J'), Modifiers::CTRL), Cmd::Newline);
    rl.bind_sequence(KeyEvent(KeyCode::Enter, Modifiers::SHIFT), Cmd::Newline);

    let history_path = dirs_home().join(".crow").join("repl_history.txt");
    let _ = std::fs::create_dir_all(history_path.parent().unwrap());
    let _ = rl.load_history(&history_path);

    let store = SessionStore::open().ok();
    if store.is_none() {
        println!(
            "  {}",
            "⚠ SessionStore unavailable — history won't persist."
                .with(Color::Yellow)
        );
    }

    let mut session = Session::new(&cfg.workspace, "Interactive REPL Session");
    println!(
        "  {} {}",
        "Session:".with(Color::DarkGrey),
        session.id.0.clone().with(Color::White)
    );
    println!(
        "  {} {}",
        "Workspace:".with(Color::DarkGrey),
        cfg.workspace.display().to_string().with(Color::White)
    );
    println!();

    let mut messages = crate::context::ConversationManager::new(vec![]);
    let mut state = SessionState::new();

    // ── Main Loop ──
    loop {
        let prompt = format!(
            "{} ",
            format!("crow:{}", state.turns).with(Color::Cyan).bold()
        );
        let readline = rl.readline(&prompt);
        match readline {
            Ok(line) => {
                let input = line.trim();
                if input.is_empty() {
                    continue;
                }

                let _ = rl.add_history_entry(input);
                let _ = rl.save_history(&history_path);

                // ── Slash Command Dispatch ──
                if let Some(cmd) = SlashCommand::parse(input) {
                    match cmd {
                        SlashCommand::Exit => {
                            println!(
                                "\n  {} Session {} ({} turns)\n",
                                "👋 Goodbye!".bold(),
                                session.id.0.clone().with(Color::DarkGrey),
                                state.turns
                            );
                            break;
                        }
                        SlashCommand::Help => {
                            print_help(&theme);
                            continue;
                        }
                        SlashCommand::Status => {
                            print_status(&session, &state, cfg, &messages);
                            continue;
                        }
                        SlashCommand::Clear => {
                            messages = crate::context::ConversationManager::new(vec![]);
                            state.turns = 0;
                            println!(
                                "  {}",
                                "🧹 Context cleared. Starting fresh."
                                    .with(Color::Green)
                            );
                            continue;
                        }
                        SlashCommand::Compact => {
                            let ctx_bytes = messages.get_total_bytes();
                            println!(
                                "  {} Compacting {} of context...",
                                "📦".to_string().with(Color::Yellow),
                                format_bytes(ctx_bytes)
                            );
                            // Trigger manual compaction if there's enough history
                            if messages.needs_compaction() || messages.as_messages().len() > 4 {
                                let summary = "User requested manual compaction of conversation history.".to_string();
                                messages.compact_into_summary(summary);
                                println!(
                                    "  {}",
                                    "✔ History compacted into summary."
                                        .with(Color::Green)
                                );
                            } else {
                                println!(
                                    "  {}",
                                    "History is already compact, nothing to do."
                                        .with(Color::DarkGrey)
                                );
                            }
                            continue;
                        }
                        SlashCommand::Unknown(name) => {
                            println!(
                                "  {} Unknown command: /{}. Type /help for available commands.",
                                "⚠".with(Color::Yellow),
                                name
                            );
                            continue;
                        }
                    }
                }

                // ── Execute Turn ──
                state.turns += 1;
                println!();
                let turn_start = Instant::now();

                match crate::run_conversation_turn(cfg, input, &mut messages).await {
                    Ok(snapshot_id) => {
                        let elapsed = turn_start.elapsed();
                        state.total_duration_ms += elapsed.as_millis();

                        // Save session
                        session.save_messages(&messages.as_messages());
                        session.push_snapshot(snapshot_id);
                        if let Some(store) = &store {
                            if let Err(e) = store.save(&session) {
                                eprintln!(
                                    "  {} Session save failed: {:?}",
                                    "⚠".with(Color::Yellow),
                                    e
                                );
                            }
                        }

                        // ── Turn Footer ──
                        print_turn_footer(elapsed, &messages, &theme);
                    }
                    Err(e) => {
                        let elapsed = turn_start.elapsed();
                        state.total_duration_ms += elapsed.as_millis();

                        println!();
                        println!(
                            "  {} {}",
                            "✘ Task failed:".bold().with(Color::Red),
                            format!("{:#}", e).with(Color::Red)
                        );
                        println!(
                            "  {}",
                            "(Provide follow-up instructions to resolve, or /clear to reset)"
                                .with(Color::DarkGrey)
                        );
                    }
                }
                println!();
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C: if the line was empty, hint they should use /exit
                println!(
                    "  {}",
                    "^C (Type /exit to leave)".with(Color::DarkGrey)
                );
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!(
                    "\n  {} Session {} ({} turns)\n",
                    "👋 Goodbye!".bold(),
                    session.id.0.clone().with(Color::DarkGrey),
                    state.turns
                );
                break;
            }
            Err(err) => {
                eprintln!("  Input error: {:?}", err);
                break;
            }
        }
    }

    Ok(())
}

// ─── Help Output ────────────────────────────────────────────────────

fn print_help(theme: &ColorTheme) {
    println!();
    println!("  {}", "Available Commands".bold().with(theme.heading));
    println!(
        "  {}",
        "───────────────────────────────────────────────"
            .with(Color::DarkGrey)
    );

    let commands = [
        ("/help", "Show this help message"),
        ("/status", "Display session status and context usage"),
        ("/clear", "Clear conversation context and start fresh"),
        ("/compact", "Manually compress conversation history"),
        ("/exit", "Exit the REPL (also: /quit, Ctrl+D)"),
    ];

    for (cmd, desc) in &commands {
        println!(
            "  {}  {}",
            format!("{:<10}", cmd).bold().with(Color::Green),
            desc.with(Color::White)
        );
    }

    println!();
    println!(
        "  {}",
        "Tips:".bold().with(theme.heading)
    );
    println!(
        "  {}",
        "• Ctrl+J or Shift+Enter inserts a newline for multi-line input"
            .with(Color::DarkGrey)
    );
    println!(
        "  {}",
        "• Tab-complete slash commands (type / then press Tab)"
            .with(Color::DarkGrey)
    );
    println!(
        "  {}",
        "• Previous commands are accessible with ↑/↓ arrow keys"
            .with(Color::DarkGrey)
    );
    println!();
}

// ─── Status Output ──────────────────────────────────────────────────

fn print_status(
    session: &Session,
    state: &SessionState,
    cfg: &CrowConfig,
    messages: &crate::context::ConversationManager,
) {
    let ctx_bytes = messages.get_total_bytes();
    let msg_count = messages.as_messages().len();
    let avg_turn_ms = if state.turns > 0 {
        state.total_duration_ms / state.turns as u128
    } else {
        0
    };

    println!();
    println!("  {}", "Session Status".bold().with(Color::Cyan));
    println!(
        "  {}",
        "───────────────────────────────────────────────"
            .with(Color::DarkGrey)
    );
    println!(
        "  {}  {}",
        "Session ID:".with(Color::DarkGrey),
        session.id.0.clone().with(Color::White)
    );
    println!(
        "  {}  {}",
        "Workspace: ".with(Color::DarkGrey),
        cfg.workspace.display().to_string().with(Color::White)
    );
    println!(
        "  {}  {}",
        "Provider:  ".with(Color::DarkGrey),
        cfg.describe_provider().with(Color::White)
    );
    println!(
        "  {}  {:?}",
        "Write Mode:".with(Color::DarkGrey),
        cfg.write_mode
    );
    println!();
    println!(
        "  {}  {}",
        "Turns:     ".with(Color::DarkGrey),
        state.turns.to_string().bold()
    );
    println!(
        "  {}  {} ({} messages)",
        "Context:   ".with(Color::DarkGrey),
        format_bytes(ctx_bytes).bold(),
        msg_count
    );
    println!(
        "  {}  {}",
        "Avg Turn:  ".with(Color::DarkGrey),
        format_duration_ms(avg_turn_ms)
    );
    println!();
}

// ─── Turn Footer ────────────────────────────────────────────────────

fn print_turn_footer(
    elapsed: std::time::Duration,
    messages: &crate::context::ConversationManager,
    _theme: &ColorTheme,
) {
    let ctx_bytes = messages.get_total_bytes();
    println!(
        "\n  {}  {}  {}",
        format!("⏱ {}", format_duration(elapsed)).with(Color::DarkGrey),
        format!("│ ctx: {}", format_bytes(ctx_bytes)).with(Color::DarkGrey),
        "│ ✔ synced".with(Color::DarkGrey),
    );
}

// ─── Formatting Helpers ─────────────────────────────────────────────

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn format_duration_ms(ms: u128) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}
