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
use rustyline::{
    Cmd, CompletionType, Config, Context, EditMode, Editor, Helper, KeyCode, KeyEvent, Modifiers,
};
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
    View(crate::event::ViewMode),
    Unknown(String),
}

impl SlashCommand {
    fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let mut parts = trimmed.trim_start_matches('/').split_whitespace();
        let command = parts.next().unwrap_or_default();
        Some(match command {
            "help" | "?" => Self::Help,
            "status" => Self::Status,
            "clear" => Self::Clear,
            "compact" => Self::Compact,
            "view" => {
                let mode_str = parts.next().unwrap_or_default().to_lowercase();
                let mode = match mode_str.as_str() {
                    "focus" => crate::event::ViewMode::Focus,
                    "evidence" => crate::event::ViewMode::Evidence,
                    "audit" => crate::event::ViewMode::Audit,
                    _ => crate::event::ViewMode::Evidence,
                };
                Self::View(mode)
            }
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
            "/view".into(),
            "/exit".into(),
        ]
    }
}

// ─── Session State ──────────────────────────────────────────────────

struct SessionState {
    turns: usize,
    total_duration_ms: u128,
    view_mode: crate::event::ViewMode,
}

impl SessionState {
    fn new() -> Self {
        Self {
            turns: 0,
            total_duration_ms: 0,
            view_mode: crate::event::ViewMode::Evidence,
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
    let mut session = Session::new(&cfg.workspace, "Interactive REPL Session");
    print_repl_banner(cfg, &theme, &session);

    // ── Editor Setup ──
    let editor_config = Config::builder()
        .completion_type(CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .build();
    let mut rl = Editor::<CrowHelper, DefaultHistory>::with_config(editor_config)?;
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
            "⚠ SessionStore unavailable — history won't persist.".with(Color::Yellow)
        );
        println!();
    }

    let mut messages = crate::context::ConversationManager::new(vec![]);
    let mut state = SessionState::new();
    let runtime = crate::runtime::SessionRuntime::boot(cfg).await?;

    // ── Main Loop ──
    loop {
        let prompt = build_prompt(&state, cfg, &messages, &theme);
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
                    let mut ctx = ReplContext {
                        session: &session,
                        state: &mut state,
                        cfg,
                        messages: &mut messages,
                        theme: &theme,
                    };
                    match handle_slash_command(cmd, &mut ctx) {
                        CommandOutcome::Break => break,
                        CommandOutcome::Continue => continue,
                    }
                }

                // ── Skip Empty Input ──
                if input.is_empty() {
                    continue;
                }

                // ── Execute Turn ──
                state.turns += 1;
                let turn_start = Instant::now();

                // Console 3.0: Top Connective Frame
                println!(
                    "\n  {} {}",
                    "╭─ Goal ›".with(Color::Cyan).bold(),
                    input.with(Color::AnsiValue(250))
                );
                // Print a visual spacer inside the frame
                println!("  {}", "│".with(Color::DarkGrey));

                match runtime
                    .execute_turn(cfg, input, &mut messages, state.view_mode)
                    .await
                {
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
                    }
                    Err(e) => {
                        let elapsed = turn_start.elapsed();
                        state.total_duration_ms += elapsed.as_millis();

                        println!();
                        println!(
                            "  {} {}",
                            "│  ✘".bold().with(Color::AnsiValue(203)),
                            format!("{:#}", e).with(Color::AnsiValue(203))
                        );
                        println!(
                            "  {} {}",
                            "│".with(Color::DarkGrey),
                            "Use follow-up instructions to recover, or /clear to reset context."
                                .with(theme.dim)
                        );
                    }
                }

                // Console 3.0: Bottom Connective Frame
                let elapsed_ms = turn_start.elapsed().as_millis();
                let ctx_bytes = format_bytes(messages.get_total_bytes());
                println!(
                    "  {}",
                    format!("╰── completed in {}ms · ctx {}", elapsed_ms, ctx_bytes)
                        .with(Color::DarkGrey)
                );
                println!();
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C: if the line was empty, hint they should use /exit
                println!("  {}", "^C (Type /exit to leave)".with(Color::DarkGrey));
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
    println!("  {}", "◩  Commands".bold().with(theme.heading));
    println!("  {}", "─".repeat(34).with(theme.dim));

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
            desc.with(Color::AnsiValue(245))
        );
    }

    println!();
    println!("  {}", "◩  Tips".bold().with(theme.heading));
    println!("  {}", "─".repeat(34).with(theme.dim));
    println!(
        "  {}",
        "• Ctrl+J or Shift+Enter inserts a newline for multi-line input".with(theme.dim)
    );
    println!(
        "  {}",
        "• Tab-complete slash commands (type / then press Tab)".with(theme.dim)
    );
    println!(
        "  {}",
        "• Previous commands are accessible with ↑/↓ arrow keys".with(theme.dim)
    );
    println!();
}

// ─── Status Output ──────────────────────────────────────────────────

fn print_status(
    session: &Session,
    state: &SessionState,
    cfg: &CrowConfig,
    messages: &crate::context::ConversationManager,
    theme: &ColorTheme,
) {
    let ctx_bytes = messages.get_total_bytes();
    let msg_count = messages.as_messages().len();
    let avg_turn_ms = if state.turns > 0 {
        state.total_duration_ms / state.turns as u128
    } else {
        0
    };

    println!();
    println!("  {}", "◩  System Status".bold().with(theme.heading));
    println!("  {}", "─".repeat(34).with(theme.dim));
    print_kv_line("Session", &session.id.0);
    print_kv_line("Workspace", &cfg.workspace.display().to_string());
    print_kv_line("Provider", &cfg.describe_provider());
    print_kv_line("Model", &compact_model_name(cfg));
    print_kv_line("Write Mode", &write_mode_badge(cfg));
    print_kv_line("Turns", &state.turns.to_string());
    print_kv_line(
        "Context",
        &format!("{} · {} messages", format_bytes(ctx_bytes), msg_count),
    );
    if state.turns > 0 {
        print_kv_line("Avg Turn", &format_duration_ms(avg_turn_ms));
    }
    println!();
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

fn format_duration_ms(ms: u128) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}
fn write_mode_badge(cfg: &CrowConfig) -> String {
    match cfg.write_mode {
        crate::config::WriteMode::SandboxOnly => "sandbox".to_string(),
        crate::config::WriteMode::WorkspaceWrite => "write".to_string(),
        crate::config::WriteMode::DangerFullAccess => "danger".to_string(),
    }
}

fn compact_model_name(cfg: &CrowConfig) -> String {
    let model = cfg.llm.model.rsplit('/').next().unwrap_or(&cfg.llm.model);
    truncate_middle(model, 22)
}

fn truncate_middle(input: &str, max_chars: usize) -> String {
    let chars: Vec<char> = input.chars().collect();
    if chars.len() <= max_chars {
        return input.to_string();
    }

    let keep_left = max_chars.saturating_sub(1) / 2;
    let keep_right = max_chars.saturating_sub(1) - keep_left;
    let left: String = chars.iter().take(keep_left).collect();
    let right: String = chars[chars.len().saturating_sub(keep_right)..]
        .iter()
        .collect();
    format!("{left}…{right}")
}

fn print_repl_banner(_cfg: &CrowConfig, theme: &ColorTheme, _session: &Session) {
    println!();
    println!(
        "  {} {}",
        "🦅 Crow Code".bold().with(theme.heading),
        "— Type /help for commands. Ctrl+J for newline.".with(theme.dim)
    );
    println!();
}

fn build_prompt(
    _state: &SessionState,
    _cfg: &CrowConfig,
    _messages: &crate::context::ConversationManager,
    theme: &ColorTheme,
) -> String {
    format!("{} ", "crow ›".bold().with(theme.heading))
}

// Print turn header removed in pursuit of minimalism.

fn print_kv_line(label: &str, value: &str) {
    println!(
        "  {} {}",
        format!("{label:>10}").with(Color::AnsiValue(242)),
        value.with(Color::White)
    );
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

enum CommandOutcome {
    Continue,
    Break,
}

struct ReplContext<'a> {
    pub session: &'a Session,
    pub state: &'a mut SessionState,
    pub cfg: &'a CrowConfig,
    pub messages: &'a mut crate::context::ConversationManager,
    pub theme: &'a ColorTheme,
}

fn handle_slash_command(cmd: SlashCommand, ctx: &mut ReplContext) -> CommandOutcome {
    match cmd {
        SlashCommand::Exit => {
            println!(
                "\n  {} Session {} ({} turns)\n",
                "👋 Goodbye!".bold(),
                ctx.session.id.0.clone().with(Color::DarkGrey),
                ctx.state.turns
            );
            CommandOutcome::Break
        }
        SlashCommand::Help => {
            print_help(ctx.theme);
            CommandOutcome::Continue
        }
        SlashCommand::Status => {
            print_status(ctx.session, ctx.state, ctx.cfg, ctx.messages, ctx.theme);
            CommandOutcome::Continue
        }
        SlashCommand::Clear => {
            *ctx.messages = crate::context::ConversationManager::new(vec![]);
            ctx.state.turns = 0;
            println!(
                "  {}",
                "🧹 Context cleared. Starting fresh.".with(Color::Green)
            );
            CommandOutcome::Continue
        }
        SlashCommand::Compact => {
            let ctx_bytes = ctx.messages.get_total_bytes();
            println!(
                "  {} Compacting {} of context...",
                "📦".to_string().with(Color::Yellow),
                format_bytes(ctx_bytes)
            );
            if ctx.messages.needs_compaction() || ctx.messages.as_messages().len() > 4 {
                let summary =
                    "User requested manual compaction of conversation history.".to_string();
                ctx.messages.compact_into_summary(summary);
                println!(
                    "  {}",
                    "✔ History compacted into summary.".with(Color::Green)
                );
            } else {
                println!(
                    "  {}",
                    "History is already compact, nothing to do.".with(Color::DarkGrey)
                );
            }
            CommandOutcome::Continue
        }
        SlashCommand::View(mode) => {
            ctx.state.view_mode = mode;
            println!(
                "  {}",
                format!("👁 View mode set to: {:?}", mode).with(Color::Green)
            );
            CommandOutcome::Continue
        }
        SlashCommand::Unknown(name) => {
            println!(
                "  {} Unknown command: /{}. Type /help for available commands.",
                "⚠".with(Color::Yellow),
                name
            );
            CommandOutcome::Continue
        }
    }
}
