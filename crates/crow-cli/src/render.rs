//! Rich terminal rendering for Crow CLI output.
//!
//! Provides markdown-to-ANSI rendering with syntax highlighting,
//! styled headings, tables, lists, links, and code blocks.
//! Inspired by claw-code's TerminalRenderer architecture.

use std::fmt::Write as FmtWrite;
use std::io::{self, Write};

use crossterm::cursor::MoveToColumn;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor, Stylize};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, queue};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::{as_24_bit_terminal_escaped, LinesWithEndings};

// ─── Color Theme ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct ColorTheme {
    pub heading: Color,
    pub emphasis: Color,
    pub strong: Color,
    pub inline_code: Color,
    pub link: Color,
    pub quote: Color,
    pub table_border: Color,
    pub code_block_border: Color,
    pub spinner_active: Color,
    pub spinner_done: Color,
    pub spinner_failed: Color,
    pub dim: Color,
}

impl Default for ColorTheme {
    fn default() -> Self {
        Self {
            heading: Color::AnsiValue(81),
            emphasis: Color::AnsiValue(176),
            strong: Color::AnsiValue(221),
            inline_code: Color::AnsiValue(114),
            link: Color::AnsiValue(75),
            quote: Color::AnsiValue(245),
            table_border: Color::AnsiValue(66),
            code_block_border: Color::AnsiValue(240),
            spinner_active: Color::AnsiValue(81),
            spinner_done: Color::AnsiValue(114),
            spinner_failed: Color::AnsiValue(203),
            dim: Color::AnsiValue(242),
        }
    }
}

// ─── Spinner ────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct Spinner {
    frame_index: usize,
}

impl Spinner {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tick(
        &mut self,
        label: &str,
        theme: &ColorTheme,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let frame = Self::FRAMES[self.frame_index % Self::FRAMES.len()];
        self.frame_index += 1;
        queue!(
            out,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(theme.spinner_active),
            Print(format!("{frame} {label}")),
            ResetColor
        )?;
        out.flush()
    }

    pub fn finish(
        &mut self,
        label: &str,
        theme: &ColorTheme,
        out: &mut impl Write,
    ) -> io::Result<()> {
        self.frame_index = 0;
        execute!(
            out,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(theme.spinner_done),
            Print(format!("✔ {label}\n")),
            ResetColor
        )?;
        out.flush()
    }

    pub fn fail(
        &mut self,
        label: &str,
        theme: &ColorTheme,
        out: &mut impl Write,
    ) -> io::Result<()> {
        self.frame_index = 0;
        execute!(
            out,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(theme.spinner_failed),
            Print(format!("✘ {label}\n")),
            ResetColor
        )?;
        out.flush()
    }
}

// ─── Render State ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum ListKind {
    Unordered,
    Ordered { next_index: u64 },
}

#[derive(Debug, Default, Clone)]
struct TableState {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_head: bool,
}

impl TableState {
    fn push_cell(&mut self) {
        let cell = self.current_cell.trim().to_string();
        self.current_row.push(cell);
        self.current_cell.clear();
    }

    fn finish_row(&mut self) {
        if self.current_row.is_empty() {
            return;
        }
        let row = std::mem::take(&mut self.current_row);
        if self.in_head {
            self.headers = row;
        } else {
            self.rows.push(row);
        }
    }
}

#[derive(Debug, Clone)]
struct LinkState {
    destination: String,
    text: String,
}

#[derive(Debug, Default, Clone)]
struct RenderState {
    emphasis: usize,
    strong: usize,
    heading_level: Option<u8>,
    quote: usize,
    list_stack: Vec<ListKind>,
    link_stack: Vec<LinkState>,
    table: Option<TableState>,
}

impl RenderState {
    fn style_text(&self, text: &str, theme: &ColorTheme) -> String {
        let mut style = text.stylize();

        if matches!(self.heading_level, Some(1 | 2)) || self.strong > 0 {
            style = style.bold();
        }
        if self.emphasis > 0 {
            style = style.italic();
        }

        if let Some(level) = self.heading_level {
            style = match level {
                1 => style.with(theme.heading),
                2 => style.with(Color::AnsiValue(255)),
                3 => style.with(Color::AnsiValue(110)),
                _ => style.with(Color::AnsiValue(250)),
            };
        } else if self.strong > 0 {
            style = style.with(theme.strong);
        } else if self.emphasis > 0 {
            style = style.with(theme.emphasis);
        }

        if self.quote > 0 {
            style = style.with(theme.quote);
        }

        format!("{style}")
    }

    fn append_raw(&mut self, output: &mut String, text: &str) {
        if let Some(link) = self.link_stack.last_mut() {
            link.text.push_str(text);
        } else if let Some(table) = self.table.as_mut() {
            table.current_cell.push_str(text);
        } else {
            output.push_str(text);
        }
    }

    fn append_styled(&mut self, output: &mut String, text: &str, theme: &ColorTheme) {
        let styled = self.style_text(text, theme);
        self.append_raw(output, &styled);
    }
}

// ─── Terminal Renderer ──────────────────────────────────────────────

#[derive(Debug)]
pub struct TerminalRenderer {
    syntax_set: SyntaxSet,
    syntax_theme: Theme,
    color_theme: ColorTheme,
}

impl Default for TerminalRenderer {
    fn default() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let syntax_theme = resolve_syntax_theme();
        Self {
            syntax_set,
            syntax_theme,
            color_theme: ColorTheme::default(),
        }
    }
}

impl TerminalRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn color_theme(&self) -> &ColorTheme {
        &self.color_theme
    }

    /// Render markdown to a styled ANSI string for terminal display.
    #[must_use]
    pub fn render_markdown(&self, markdown: &str) -> String {
        let normalized = normalize_nested_fences(markdown);
        let mut output = String::new();
        let mut state = RenderState::default();
        let mut code_language = String::new();
        let mut code_buffer = String::new();
        let mut in_code_block = false;

        for event in Parser::new_ext(&normalized, Options::all()) {
            self.render_event(
                event,
                &mut state,
                &mut output,
                &mut code_buffer,
                &mut code_language,
                &mut in_code_block,
            );
        }

        output.trim_end().to_string()
    }

    /// Print markdown to stdout with full styling.
    pub fn print_markdown(&self, markdown: &str) {
        let rendered = self.render_markdown(markdown);
        println!("{rendered}");
    }

    /// Stream markdown rendering to a writer.
    pub fn stream_markdown(&self, markdown: &str, out: &mut impl Write) -> io::Result<()> {
        let rendered = self.render_markdown(markdown);
        write!(out, "{rendered}")?;
        if !rendered.ends_with('\n') {
            writeln!(out)?;
        }
        out.flush()
    }

    #[allow(clippy::too_many_lines)]
    fn render_event(
        &self,
        event: Event<'_>,
        state: &mut RenderState,
        output: &mut String,
        code_buffer: &mut String,
        code_language: &mut String,
        in_code_block: &mut bool,
    ) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                Self::start_heading(state, level as u8, output);
            }
            Event::End(TagEnd::Paragraph) => output.push_str("\n\n"),
            Event::Start(Tag::BlockQuote(..)) => self.start_quote(state, output),
            Event::End(TagEnd::BlockQuote(..)) => {
                state.quote = state.quote.saturating_sub(1);
                output.push('\n');
            }
            Event::End(TagEnd::Heading(..)) => {
                state.heading_level = None;
                output.push_str("\n\n");
            }
            Event::End(TagEnd::Item) | Event::SoftBreak | Event::HardBreak => {
                state.append_raw(output, "\n");
            }
            Event::Start(Tag::List(first_item)) => {
                let kind = match first_item {
                    Some(index) => ListKind::Ordered { next_index: index },
                    None => ListKind::Unordered,
                };
                state.list_stack.push(kind);
            }
            Event::End(TagEnd::List(..)) => {
                state.list_stack.pop();
                output.push('\n');
            }
            Event::Start(Tag::Item) => Self::start_item(state, output),
            Event::Start(Tag::CodeBlock(kind)) => {
                *in_code_block = true;
                *code_language = match kind {
                    CodeBlockKind::Indented => String::from("text"),
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                };
                code_buffer.clear();
                self.start_code_block(code_language, output);
            }
            Event::End(TagEnd::CodeBlock) => {
                self.finish_code_block(code_buffer, code_language, output);
                *in_code_block = false;
                code_language.clear();
                code_buffer.clear();
            }
            Event::Start(Tag::Emphasis) => state.emphasis += 1,
            Event::End(TagEnd::Emphasis) => state.emphasis = state.emphasis.saturating_sub(1),
            Event::Start(Tag::Strong) => state.strong += 1,
            Event::End(TagEnd::Strong) => state.strong = state.strong.saturating_sub(1),
            Event::Code(code) => {
                let rendered =
                    format!("{}", format!("`{code}`").with(self.color_theme.inline_code));
                state.append_raw(output, &rendered);
            }
            Event::Rule => output.push_str("───────────────────────────────────────\n"),
            Event::Text(text) => {
                self.push_text(text.as_ref(), state, output, code_buffer, *in_code_block);
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                state.append_raw(output, &html);
            }
            Event::FootnoteReference(reference) => {
                state.append_raw(output, &format!("[{reference}]"));
            }
            Event::TaskListMarker(done) => {
                state.append_raw(output, if done { "[x] " } else { "[ ] " });
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                state.append_raw(output, &math);
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                state.link_stack.push(LinkState {
                    destination: dest_url.to_string(),
                    text: String::new(),
                });
            }
            Event::End(TagEnd::Link) => {
                if let Some(link) = state.link_stack.pop() {
                    let label = if link.text.is_empty() {
                        link.destination.clone()
                    } else {
                        link.text
                    };
                    let rendered = format!(
                        "{}",
                        format!("[{label}]({})", link.destination)
                            .underlined()
                            .with(self.color_theme.link)
                    );
                    state.append_raw(output, &rendered);
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                let rendered = format!(
                    "{}",
                    format!("[image:{dest_url}]").with(self.color_theme.link)
                );
                state.append_raw(output, &rendered);
            }
            Event::Start(Tag::Table(..)) => state.table = Some(TableState::default()),
            Event::End(TagEnd::Table) => {
                if let Some(table) = state.table.take() {
                    output.push_str(&self.render_table(&table));
                    output.push_str("\n\n");
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(table) = state.table.as_mut() {
                    table.in_head = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(table) = state.table.as_mut() {
                    table.finish_row();
                    table.in_head = false;
                }
            }
            Event::Start(Tag::TableRow) => {
                if let Some(table) = state.table.as_mut() {
                    table.current_row.clear();
                    table.current_cell.clear();
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(table) = state.table.as_mut() {
                    table.finish_row();
                }
            }
            Event::Start(Tag::TableCell) => {
                if let Some(table) = state.table.as_mut() {
                    table.current_cell.clear();
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(table) = state.table.as_mut() {
                    table.push_cell();
                }
            }
            Event::Start(Tag::Paragraph | Tag::MetadataBlock(..) | _)
            | Event::End(TagEnd::Image | TagEnd::MetadataBlock(..) | _) => {}
        }
    }

    fn start_heading(state: &mut RenderState, level: u8, output: &mut String) {
        state.heading_level = Some(level);
        if !output.is_empty() {
            output.push('\n');
        }
    }

    fn start_quote(&self, state: &mut RenderState, output: &mut String) {
        state.quote += 1;
        let _ = write!(output, "{}", "│ ".with(self.color_theme.quote));
    }

    fn start_item(state: &mut RenderState, output: &mut String) {
        let depth = state.list_stack.len().saturating_sub(1);
        output.push_str(&"  ".repeat(depth));

        let marker = match state.list_stack.last_mut() {
            Some(ListKind::Ordered { next_index }) => {
                let value = *next_index;
                *next_index += 1;
                format!("{value}. ")
            }
            _ => "• ".to_string(),
        };
        output.push_str(&marker);
    }

    fn start_code_block(&self, code_language: &str, output: &mut String) {
        let label = if code_language.is_empty() {
            "code".to_string()
        } else {
            code_language.to_string()
        };
        let _ = writeln!(
            output,
            "{}",
            format!("╭─ {label}")
                .bold()
                .with(self.color_theme.code_block_border)
        );
    }

    fn finish_code_block(&self, code_buffer: &str, code_language: &str, output: &mut String) {
        output.push_str(&self.highlight_code(code_buffer, code_language));
        let _ = write!(
            output,
            "{}",
            "╰─".bold().with(self.color_theme.code_block_border)
        );
        output.push_str("\n\n");
    }

    fn push_text(
        &self,
        text: &str,
        state: &mut RenderState,
        output: &mut String,
        code_buffer: &mut String,
        in_code_block: bool,
    ) {
        if in_code_block {
            code_buffer.push_str(text);
        } else {
            state.append_styled(output, text, &self.color_theme);
        }
    }

    fn render_table(&self, table: &TableState) -> String {
        let mut rows = Vec::new();
        if !table.headers.is_empty() {
            rows.push(table.headers.clone());
        }
        rows.extend(table.rows.iter().cloned());

        if rows.is_empty() {
            return String::new();
        }

        let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths = (0..column_count)
            .map(|column| {
                rows.iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| visible_width(cell))
                    .max()
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();

        let border = format!("{}", "│".with(self.color_theme.table_border));
        let separator = widths
            .iter()
            .map(|width| "─".repeat(*width + 2))
            .collect::<Vec<_>>()
            .join(&format!("{}", "┼".with(self.color_theme.table_border)));
        let separator = format!("{border}{separator}{border}");

        let mut output = String::new();
        if !table.headers.is_empty() {
            output.push_str(&self.render_table_row(&table.headers, &widths, true));
            output.push('\n');
            output.push_str(&separator);
            if !table.rows.is_empty() {
                output.push('\n');
            }
        }

        for (index, row) in table.rows.iter().enumerate() {
            output.push_str(&self.render_table_row(row, &widths, false));
            if index + 1 < table.rows.len() {
                output.push('\n');
            }
        }

        output
    }

    fn render_table_row(&self, row: &[String], widths: &[usize], is_header: bool) -> String {
        let border = format!("{}", "│".with(self.color_theme.table_border));
        let mut line = String::new();
        line.push_str(&border);

        for (index, width) in widths.iter().enumerate() {
            let cell = row.get(index).map_or("", String::as_str);
            line.push(' ');
            if is_header {
                let _ = write!(line, "{}", cell.bold().with(self.color_theme.heading));
            } else {
                line.push_str(cell);
            }
            let padding = width.saturating_sub(visible_width(cell));
            line.push_str(&" ".repeat(padding + 1));
            line.push_str(&border);
        }

        line
    }

    /// Syntax-highlight a code block.
    #[must_use]
    pub fn highlight_code(&self, code: &str, language: &str) -> String {
        let syntax = self
            .syntax_set
            .find_syntax_by_token(language)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        let mut syntax_highlighter = HighlightLines::new(syntax, &self.syntax_theme);
        let mut colored_output = String::new();

        for line in LinesWithEndings::from(code) {
            match syntax_highlighter.highlight_line(line, &self.syntax_set) {
                Ok(ranges) => {
                    let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
                    colored_output.push_str(&apply_code_block_background(&escaped));
                }
                Err(_) => colored_output.push_str(&apply_code_block_background(line)),
            }
        }

        colored_output
    }
}

// ─── Streaming Markdown State (Codex-inspired newline-gated commit) ──

/// Newline-gated accumulator that renders markdown and commits only fully
/// completed logical lines. Ported from codex's `MarkdownStreamCollector`.
///
/// Key differences from the previous "safe boundary" approach:
/// - Only commits lines terminated by `\n` (prevents partial-line artifacts)
/// - Tracks `committed_line_count` to emit only *new* lines (no duplicates)
/// - Built-in JSON plan filtering (suppresses `{"action":"submit_plan",...}`)
/// - Clear `commit_complete_lines` / `finalize_and_drain` semantics
#[derive(Debug, Default, Clone)]
pub struct MarkdownStreamState {
    buffer: String,
    committed_line_count: usize,
}

impl MarkdownStreamState {
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.committed_line_count = 0;
    }

    /// Push a new text delta. Returns rendered ANSI lines for any newly
    /// completed logical lines (newline-gated). Returns None if no new
    /// complete lines are available yet.
    #[must_use]
    pub fn push(&mut self, renderer: &TerminalRenderer, delta: &str) -> Option<String> {
        self.buffer.push_str(delta);

        // Only commit up to the last newline (codex pattern)
        let last_newline_idx = self.buffer.rfind('\n')?;
        let source = self.buffer[..=last_newline_idx].to_string();

        // Filter: if the accumulated buffer looks like raw JSON plan output,
        // suppress it entirely. The rationale is extracted separately via
        // AgentEvent::Markdown.
        if is_json_plan_output(&source) {
            return None;
        }

        let rendered = renderer.render_markdown(&source);
        let rendered_lines: Vec<&str> = rendered.lines().collect();
        let complete_line_count = rendered_lines.len();

        if self.committed_line_count >= complete_line_count {
            return None;
        }

        let new_lines = &rendered_lines[self.committed_line_count..complete_line_count];
        self.committed_line_count = complete_line_count;

        if new_lines.is_empty() {
            None
        } else {
            Some(new_lines.join("\n"))
        }
    }

    /// Finalize the stream: emit all remaining lines beyond the last commit.
    /// If the buffer does not end with a newline, a temporary one is appended
    /// for rendering.
    #[must_use]
    pub fn flush(&mut self, renderer: &TerminalRenderer) -> Option<String> {
        if self.buffer.trim().is_empty() {
            self.clear();
            return None;
        }

        // Filter JSON plan output at flush time too
        if is_json_plan_output(&self.buffer) {
            self.clear();
            return None;
        }

        let mut source = self.buffer.clone();
        if !source.ends_with('\n') {
            source.push('\n');
        }

        let rendered = renderer.render_markdown(&source);
        let rendered_lines: Vec<&str> = rendered.lines().collect();

        let out = if self.committed_line_count >= rendered_lines.len() {
            None
        } else {
            let new_lines = &rendered_lines[self.committed_line_count..];
            if new_lines.is_empty() {
                None
            } else {
                Some(new_lines.join("\n"))
            }
        };

        self.clear();
        out
    }
}

/// Returns true if the text looks like a raw JSON plan output from the LLM.
/// This covers `{"action":"submit_plan",...}` and similar internal JSON
/// that should not be displayed to the user.
fn is_json_plan_output(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with('{') && trimmed.contains("\"action\"")
}

// ─── Utility Helpers ────────────────────────────────────────────────

fn resolve_syntax_theme() -> Theme {
    let mut themes = ThemeSet::load_defaults().themes;
    let requested = std::env::var("CROW_SYNTAX_THEME").ok();

    if let Some(theme_name) = requested.as_deref().and_then(resolve_theme_alias) {
        if let Some(theme) = themes.remove(theme_name) {
            return theme;
        }
    }

    themes.remove("base16-ocean.dark").unwrap_or_default()
}

fn resolve_theme_alias(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "ocean" | "ocean-dark" | "base16-ocean" => Some("base16-ocean.dark"),
        "ocean-light" => Some("base16-ocean.light"),
        "github" | "light" | "inspired-github" => Some("InspiredGitHub"),
        "solarized-dark" => Some("Solarized (dark)"),
        "solarized-light" => Some("Solarized (light)"),
        "" => None,
        _ => None,
    }
}

fn apply_code_block_background(line: &str) -> String {
    let trimmed = line.trim_end_matches('\n');
    let trailing_newline = if trimmed.len() == line.len() {
        ""
    } else {
        "\n"
    };
    let with_background = trimmed.replace("\u{1b}[0m", "\u{1b}[0;48;5;236m");
    format!("\u{1b}[48;5;236m{with_background}\u{1b}[0m{trailing_newline}")
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::expect_used)]
fn normalize_nested_fences(markdown: &str) -> String {
    #[derive(Debug, Clone)]
    struct FenceLine {
        character: char,
        length: usize,
        has_info: bool,
        indent: usize,
    }

    fn parse_fence_line(line: &str) -> Option<FenceLine> {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        let indent = trimmed.chars().take_while(|c| *c == ' ').count();
        if indent > 3 {
            return None;
        }

        let rest = &trimmed[indent..];
        let character = rest.chars().next()?;
        if character != '`' && character != '~' {
            return None;
        }

        let length = rest.chars().take_while(|c| *c == character).count();
        if length < 3 {
            return None;
        }

        let after = &rest[length..];
        if character == '`' && after.contains('`') {
            return None;
        }

        Some(FenceLine {
            character,
            length,
            has_info: !after.trim().is_empty(),
            indent,
        })
    }

    let lines: Vec<&str> = markdown.split_inclusive('\n').collect();
    let fence_info: Vec<Option<FenceLine>> =
        lines.iter().map(|line| parse_fence_line(line)).collect();

    struct StackEntry {
        line_idx: usize,
        fence: FenceLine,
    }

    let mut stack = Vec::new();
    let mut pairs = Vec::new();

    for (idx, fence) in fence_info.iter().enumerate() {
        let Some(fence) = fence else { continue };

        if fence.has_info {
            stack.push(StackEntry {
                line_idx: idx,
                fence: fence.clone(),
            });
            continue;
        }

        let closes_top = stack.last().is_some_and(|top| {
            top.fence.character == fence.character && fence.length >= top.fence.length
        });

        if closes_top {
            let opener = stack.pop().expect("stack must contain opener");
            let inner_max = fence_info[opener.line_idx + 1..idx]
                .iter()
                .filter_map(|candidate| candidate.as_ref().map(|f| f.length))
                .max()
                .unwrap_or(0);
            pairs.push((opener.line_idx, idx, inner_max));
        } else {
            stack.push(StackEntry {
                line_idx: idx,
                fence: fence.clone(),
            });
        }
    }

    struct Rewrite {
        character: char,
        new_length: usize,
        indent: usize,
    }

    let mut rewrites = std::collections::HashMap::new();
    for (opener_idx, closer_idx, inner_max) in pairs {
        let opener = fence_info[opener_idx]
            .as_ref()
            .expect("paired opener must exist");
        if opener.length > inner_max {
            continue;
        }

        let new_length = inner_max + 1;
        rewrites.insert(
            opener_idx,
            Rewrite {
                character: opener.character,
                new_length,
                indent: opener.indent,
            },
        );

        let closer = fence_info[closer_idx]
            .as_ref()
            .expect("paired closer must exist");
        rewrites.insert(
            closer_idx,
            Rewrite {
                character: closer.character,
                new_length,
                indent: closer.indent,
            },
        );
    }

    if rewrites.is_empty() {
        return markdown.to_string();
    }

    let mut output = String::with_capacity(markdown.len() + rewrites.len() * 4);
    for (idx, line) in lines.iter().enumerate() {
        if let Some(rewrite) = rewrites.get(&idx) {
            let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
            let fence = fence_info[idx]
                .as_ref()
                .expect("rewrite target must be fence");
            let after = &trimmed[fence.indent + fence.length..];
            let trailing = &line[trimmed.len()..];

            output.push_str(&" ".repeat(rewrite.indent));
            output.push_str(&rewrite.character.to_string().repeat(rewrite.new_length));
            output.push_str(after);
            output.push_str(trailing);
        } else {
            output.push_str(line);
        }
    }

    output
}

fn visible_width(input: &str) -> usize {
    strip_ansi(input).chars().count()
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            output.push(ch);
        }
    }

    output
}

// ─── Convenience: Token Usage Formatting ────────────────────────────

/// Format a token count with thousands separators for readability.
pub fn format_tokens(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Format a cost in USD with 4 decimal places.
pub fn format_cost(usd: f64) -> String {
    if usd < 0.01 {
        format!("${usd:.4}")
    } else {
        format!("${usd:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_markdown_with_styling() {
        let r = TerminalRenderer::new();
        let out =
            r.render_markdown("# Hello\n\nThis is **bold** and *italic*.\n\n- item\n\n`code`");
        assert!(out.contains("Hello"));
        assert!(out.contains("• item"));
        assert!(out.contains('\u{1b}')); // ANSI escape
    }

    #[test]
    fn renders_code_blocks_with_syntax_highlighting() {
        let r = TerminalRenderer::new();
        let out = r.render_markdown("```rust\nfn main() {}\n```");
        let plain = strip_ansi(&out);
        assert!(plain.contains("╭─ rust"));
        assert!(plain.contains("fn main()"));
    }

    #[test]
    fn renders_tables() {
        let r = TerminalRenderer::new();
        let out = r.render_markdown("| A | B |\n|---|---|\n| 1 | 2 |");
        let plain = strip_ansi(&out);
        assert!(plain.contains("│"));
        assert!(plain.contains("A"));
        assert!(plain.contains("1"));
    }

    #[test]
    fn format_tokens_adds_separators() {
        assert_eq!(format_tokens(42), "42");
        assert_eq!(format_tokens(1_234), "1,234");
        assert_eq!(format_tokens(1_234_567), "1,234,567");
    }

    #[test]
    fn streaming_buffers_until_safe_boundary() {
        let r = TerminalRenderer::new();
        let mut state = MarkdownStreamState::default();
        assert!(state.push(&r, "# Heading").is_none());
        assert!(state.push(&r, "\n\nParagraph\n\n").is_some());
    }

    #[test]
    fn renders_nested_fenced_code_blocks() {
        let r = TerminalRenderer::new();
        let out = r.render_markdown("````markdown\n```rust\nfn nested() {}\n```\n````");
        let plain = strip_ansi(&out);
        assert!(plain.contains("╭─ markdown"));
        assert!(plain.contains("```rust"));
        assert!(plain.contains("fn nested()"));
    }

    #[test]
    fn streaming_newline_gated_commits_incrementally() {
        // Codex pattern: lines are committed as soon as they end with \n.
        // No fence-tracking needed — the markdown renderer handles fences.
        let r = TerminalRenderer::new();
        let mut state = MarkdownStreamState::default();

        // First push has multiple newlines — should commit completed lines
        let result = state.push(&r, "````markdown\n```rust\nfn inner() {}\n");
        assert!(result.is_some(), "newline-terminated content should commit");

        // More content with newline
        let result2 = state.push(&r, "```\n");
        // May or may not have new lines depending on what was already committed
        // Just verify no panic

        // Closing fence
        let result3 = state.push(&r, "````\n");
        // Verify flush works cleanly
        let flushed = state.flush(&r);
        // All content should have been committed by now
        drop((result2, result3, flushed));
    }
}
