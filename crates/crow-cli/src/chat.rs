use crate::config::CrowConfig;
use crate::render::{ColorTheme, TerminalRenderer};
use crate::session::{Session, SessionStore};
use anyhow::Result;
use crossterm::{
    style::{Color, Stylize},
    terminal::size,
};
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
    let mut runtime = crate::runtime::SessionRuntime::boot(cfg).await?;

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
                print_turn_header(state.turns, input, cfg, &messages, &theme);
                let turn_start = Instant::now();

                match runtime.execute_turn(cfg, input, &mut messages).await {
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
                            "✘".bold().with(Color::AnsiValue(203)),
                            format!("{:#}", e).with(Color::AnsiValue(203))
                        );
                        println!(
                            "  {}",
                            "Use follow-up instructions to recover, or /clear to reset context."
                                .with(theme.dim)
                        );
                    }
                }
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
    print_section_title("Commands", "Interactive REPL shortcuts", theme);

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
    print_section_title("Tips", "Input ergonomics", theme);
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
    print_section_title("Session", "Current REPL state", theme);
    print_kv_line("Session", &session.id.0);
    print_kv_line("Workspace", &cfg.workspace.display().to_string());
    print_kv_line("Provider", &cfg.describe_provider());
    print_kv_line("Write Mode", &write_mode_badge(cfg));
    print_kv_line("Turns", &state.turns.to_string());
    print_kv_line(
        "Context",
        &format!("{} · {} messages", format_bytes(ctx_bytes), msg_count),
    );
    print_kv_line("Avg Turn", &format_duration_ms(avg_turn_ms));
    println!();
}

// ─── Turn Footer ────────────────────────────────────────────────────

fn print_turn_footer(
    elapsed: std::time::Duration,
    messages: &crate::context::ConversationManager,
    theme: &ColorTheme,
) {
    let ctx_bytes = messages.get_total_bytes();
    let msg_count = messages.as_messages().len();
    println!(
        "\n  {}",
        format!(
            "╰─ Completed in {} · ctx {} · {} messages · synced",
            format_duration(elapsed),
            format_bytes(ctx_bytes),
            msg_count
        )
        .with(theme.dim),
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

fn terminal_width() -> usize {
    size().map(|(w, _)| usize::from(w)).unwrap_or(80)
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

fn compact_preview(input: &str, max_chars: usize) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_middle(&normalized, max_chars)
}

fn print_repl_banner(cfg: &CrowConfig, theme: &ColorTheme, session: &Session) {
    println!();
    print_section_title("Crow Code", "Evidence-driven terminal coding agent", theme);
    print_kv_line("Model", &compact_model_name(cfg));
    print_kv_line("Provider", &cfg.describe_provider());
    print_kv_line("Workspace", &cfg.workspace.display().to_string());
    print_kv_line("Write Mode", &write_mode_badge(cfg));
    print_kv_line("Session", &session.id.0);
    println!(
        "  {}",
        "Type /help for commands. Ctrl+J or Shift+Enter inserts a newline.".with(theme.dim)
    );
    println!();
}

fn build_prompt(
    state: &SessionState,
    cfg: &CrowConfig,
    messages: &crate::context::ConversationManager,
    theme: &ColorTheme,
) -> String {
    let turn = state.turns + 1;
    let status = format!(
        "{} · {} · {}",
        compact_model_name(cfg),
        write_mode_badge(cfg),
        format_bytes(messages.get_total_bytes())
    );

    format!(
        "{} {} {} ",
        format!("crow:{turn:02}").bold().with(theme.heading),
        status.with(Color::AnsiValue(245)),
        "›".bold().with(theme.heading),
    )
}

fn print_turn_header(
    turn: usize,
    input: &str,
    cfg: &CrowConfig,
    messages: &crate::context::ConversationManager,
    theme: &ColorTheme,
) {
    let meta = format!(
        "Turn {:02} · {} · {} · ctx {}",
        turn,
        compact_model_name(cfg),
        write_mode_badge(cfg),
        format_bytes(messages.get_total_bytes())
    );
    let preview = compact_preview(input, terminal_width().saturating_sub(12).max(24));

    println!();
    println!("  {}", format!("╭─ {meta}").bold().with(theme.heading));
    println!(
        "  {} {}",
        "›".bold().with(theme.emphasis),
        preview.with(Color::White)
    );
    println!("  {}", "╰─ analyzing workspace".with(theme.dim));
}

fn print_section_title(title: &str, subtitle: &str, theme: &ColorTheme) {
    let width = terminal_width().saturating_sub(6).max(18);
    let title_blob = format!(" {title} ");
    let rule_len = width.saturating_sub(title_blob.chars().count());
    println!(
        "  {}{}",
        title_blob.bold().with(theme.heading),
        "─".repeat(rule_len).with(theme.dim)
    );
    println!("  {}", subtitle.with(theme.dim));
}

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
