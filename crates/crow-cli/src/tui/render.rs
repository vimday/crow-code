use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use super::state::{AppState, CellKind, OverlayState};
use super::theme::{chars, colors, Styles};

/// Left gutter width matching Codex's LIVE_PREFIX_COLS.
const GUTTER: &str = "  ";

// ── Color aliases from theme (backwards compat for cells) ────────────────────
fn dim_gray() -> Color {
    colors::text_muted()
}
fn mid_gray() -> Color {
    colors::text_secondary()
}
fn accent_cyan() -> Color {
    colors::accent_system()
}
fn accent_green() -> Color {
    colors::accent_success()
}
fn accent_red() -> Color {
    colors::accent_error()
}
fn verdict_blue() -> Color {
    colors::accent_user()
}

// ── Spinner frames from theme ────────────────────────────────────────────────
const SPINNER: &[&str] = chars::SPINNER;

pub fn render_app(f: &mut Frame, state: &mut AppState) {
    let size = f.size();

    let composer_lines = if matches!(
        state.approval_state,
        crate::tui::state::ApprovalState::PendingCommand(_)
    ) {
        3 // 3 lines of text for approval + 1 border = 4 total length below
    } else {
        state.composer.lines().count().max(1) as u16
    };

    let swarm_lines = if state.active_swarms.is_empty() { 0 } else { 1 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),                     // Conversation pane
            Constraint::Length(swarm_lines),        // Swarm bar
            Constraint::Length(1),                  // Status bar
            Constraint::Length(composer_lines + 1), // Composer + top border
        ])
        .split(size);

    render_history(f, state, chunks[0]);
    if swarm_lines > 0 {
        render_swarm_bar(f, state, chunks[1]);
    }
    render_status_bar(f, state, chunks[2]);
    render_composer(f, state, chunks[3]);
}

// ── Conversation Pane ────────────────────────────────────────────────────────

pub trait HistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;
}

struct UserCell<'a>(&'a str);
impl<'a> HistoryCell for UserCell<'a> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from("")];
        let wrap_width = width.saturating_sub(4).max(1) as usize;
        let wrapped = textwrap::wrap(self.0, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 { "› " } else { "  " };
            lines.push(Line::from(vec![
                prefix.white().bold().dim(),
                line.to_string().white(),
            ]));
        }
        lines.push(Line::from(""));
        lines
    }
}

struct AgentMessageCell<'a>(&'a str);
impl<'a> HistoryCell for AgentMessageCell<'a> {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        // Use the streaming markdown renderer for rich output
        let mut renderer = super::markdown_stream::StreamingMarkdownRenderer::new();
        let md_lines = renderer.set_content(self.0.to_string());
        let mut out = Vec::new();
        for (i, line) in md_lines.iter().enumerate() {
            let prefix = if i == 0 { "• " } else { "  " };
            let mut spans = vec![Span::styled(prefix, Styles::evidence())];
            spans.extend(line.spans.iter().cloned());
            out.push(Line::from(spans));
        }
        if out.is_empty() {
            // Fallback for empty content
            out.push(Line::from(vec![
                "• ".fg(dim_gray()).dim(),
                self.0.to_string().fg(colors::text_primary()),
            ]));
        }
        out
    }
}

struct EvidenceCell<'a>(&'a str);
impl<'a> HistoryCell for EvidenceCell<'a> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(self.0, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}◎ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.fg(mid_gray()),
                line.to_string().fg(mid_gray()).dim(),
            ]));
        }
        lines
    }
}

struct ActionCell<'a>(&'a str);
impl<'a> HistoryCell for ActionCell<'a> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(self.0, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}▰ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.fg(accent_green()),
                line.to_string().fg(accent_green()),
            ]));
        }
        lines
    }
}

struct ResultCell<'a>(&'a str);
impl<'a> HistoryCell for ResultCell<'a> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(self.0, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}✓ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.fg(verdict_blue()),
                line.to_string().fg(verdict_blue()),
            ]));
        }
        lines
    }
}

struct LogCell<'a>(&'a str);
impl<'a> HistoryCell for LogCell<'a> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(self.0, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}• ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.fg(dim_gray()),
                line.to_string().fg(mid_gray()),
            ]));
        }
        lines
    }
}

struct ErrorCell<'a>(&'a str);
impl<'a> HistoryCell for ErrorCell<'a> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(self.0, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}✘ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.fg(accent_red()).bold(),
                line.to_string().fg(accent_red()),
            ]));
        }
        lines
    }
}

fn render_history(f: &mut Frame, state: &AppState, area: Rect) {
    let viewport = area.height as usize;
    if viewport == 0 {
        return;
    }

    let mut reversed_items: Vec<ListItem> = Vec::new();
    let mut to_skip = state.scroll_offset;
    let mut to_take = viewport;

    macro_rules! push_item {
        ($item:expr) => {
            if to_skip > 0 {
                to_skip -= 1;
            } else if to_take > 0 {
                reversed_items.push($item);
                to_take -= 1;
            }
        };
    }

    // 1. Active spinner is at the very bottom
    if let Some(action) = &state.active_action {
        let frame = SPINNER[state.spinner_idx % SPINNER.len()];
        let mut lines = Vec::new();
        let wrap_width = area.width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(action, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}{frame} ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.fg(accent_cyan()),
                line.to_string().fg(accent_cyan()),
            ]));
        }
        for item in lines.into_iter().rev() {
            push_item!(ListItem::new(item));
        }
    }

    // 2. Iterate history backwards using HistoryCell implementations
    for cell in state.history.iter().rev() {
        if to_take == 0 {
            break;
        }

        let history_cell: Box<dyn HistoryCell> = match cell.kind {
            CellKind::User => Box::new(UserCell(&cell.payload)),
            CellKind::AgentMessage => Box::new(AgentMessageCell(&cell.payload)),
            CellKind::Evidence => Box::new(EvidenceCell(&cell.payload)),
            CellKind::Action => Box::new(ActionCell(&cell.payload)),
            CellKind::Result => Box::new(ResultCell(&cell.payload)),
            CellKind::Log => Box::new(LogCell(&cell.payload)),
            CellKind::Error => Box::new(ErrorCell(&cell.payload)),
        };

        let lines = history_cell.display_lines(area.width);

        // Send this cell's lines backwards into our virtualized view
        for item in lines.into_iter().rev() {
            push_item!(ListItem::new(item));
        }
    }

    // 3. If we didn't fill the viewport, pad with empty lines
    let mut items: Vec<ListItem> = reversed_items.into_iter().rev().collect();
    if to_take > 0 {
        let mut padded = vec![ListItem::new(Line::from("")); to_take];
        padded.extend(items);
        items = padded;
    }

    let list = List::new(items).block(Block::default().borders(Borders::NONE));
    f.render_widget(list, area);
}

// ── Swarm Bar ────────────────────────────────────────────────────────────────
fn render_swarm_bar(f: &mut Frame, state: &AppState, area: Rect) {
    if area.width < 4 || state.active_swarms.is_empty() {
        return;
    }

    let mut spans = vec!["⚡ Swarm Active | ".yellow().bold()];

    let frame = SPINNER[state.spinner_idx % SPINNER.len()];

    for (i, (id, task)) in state.active_swarms.iter().enumerate() {
        let display_task = if task.len() > 30 {
            format!("{}...", &task[..27])
        } else {
            task.clone()
        };
        spans.push(format!("{frame}{id} [{display_task}]").cyan());
        if i < state.active_swarms.len() - 1 {
            spans.push(Span::raw("   "));
        }
    }

    let p = Paragraph::new(Line::from(spans))
        .style(ratatui::style::Style::default().bg(Color::Indexed(236)));
    f.render_widget(p, area);
}

// ── Status Bar ───────────────────────────────────────────────────────────────
// Codex pattern: left-side hints, right-side context joined by ` · `,
// connected by `─` fill.

fn render_status_bar(f: &mut Frame, state: &AppState, area: Rect) {
    if area.width < 4 {
        return;
    }

    // Left side: mode hint + task/branch info
    let left = if state.is_task_running() {
        " esc to interrupt ".to_string()
    } else {
        " ? for help ".to_string()
    };

    let git_info = if state.is_dirty {
        format!(" {}*", state.git_branch)
    } else {
        format!(" {}", state.git_branch)
    };

    let risk_color = if state.is_dirty {
        accent_red()
    } else {
        dim_gray()
    };

    // Right side: model · workspace · view mode · write mode
    let mut right_parts: Vec<String> = Vec::new();
    right_parts.push(state.model_info.clone());
    if !state.workspace_name.is_empty() {
        right_parts.push(state.workspace_name.clone());
    }
    right_parts.push(format!("{:?}", state.view_mode));
    right_parts.push(state.write_mode.clone());

    let right = format!(" {} ", right_parts.join(" · "));

    let left_span = Line::from(vec![left.fg(dim_gray()), git_info.fg(risk_color)]);

    let left_w = left_span.width().min(area.width as usize);
    let right_w = right.chars().count().min(area.width as usize);

    let status_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_w as u16 + 2), // Buffer for branch
            Constraint::Min(0),
            Constraint::Length(right_w as u16),
        ])
        .split(area);

    let left_widget = Paragraph::new(left_span);
    let right_widget = Paragraph::new(right.fg(dim_gray()));

    // Fill middle with `─`
    let mid_w = status_chunks[1].width as usize;
    let mid_fill = "─".repeat(mid_w);
    let mid_widget = Paragraph::new(mid_fill.fg(Color::Indexed(236)));

    f.render_widget(left_widget, status_chunks[0]);
    f.render_widget(mid_widget, status_chunks[1]);
    f.render_widget(right_widget, status_chunks[2]);
}

// ── Composer ─────────────────────────────────────────────────────────────────
// Codex pattern: top border only, left gutter aligned, `❯ ` prompt.

fn render_composer(f: &mut Frame, state: &AppState, area: Rect) {
    let mut composer_lines = Vec::new();

    // If there is a pending command approval, render the security prompt instead
    if let crate::tui::state::ApprovalState::PendingCommand(ref cmd) = state.approval_state {
        let warning_style = Styles::error();
        composer_lines.push(Line::from(vec![Span::styled(
            "⚠️  Security Approval Required",
            warning_style,
        )]));
        composer_lines.push(Line::from(vec![
            "Command: ".fg(dim_gray()),
            cmd.clone().into(),
        ]));
        composer_lines.push(Line::from(vec![
            "Execute this command? [y/N/a (always)]: "
                .fg(Color::Indexed(221))
                .bold(),
            "█".fg(accent_cyan()),
        ]));

        let composer_widget = Paragraph::new(composer_lines).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Styles::error()),
        );

        f.render_widget(composer_widget, area);
        return;
    }

    let text = state.composer.clone();
    let cursor_idx = state.composer_cursor.min(text.chars().count());

    let (before_cursor, after_cursor) = if text.is_empty() {
        ("".to_string(), "".to_string())
    } else {
        let byte_idx = text.chars().take(cursor_idx).map(char::len_utf8).sum();
        let b = text[..byte_idx].to_string();
        let a = text[byte_idx..].to_string();
        (b, a)
    };

    let before_lines: Vec<&str> = before_cursor.split_inclusive('\n').collect();
    let after_lines: Vec<&str> = after_cursor.split_inclusive('\n').collect();

    let is_running = state.is_task_running();
    let prompt_color = if is_running {
        dim_gray()
    } else {
        colors::accent_success()
    };
    let block_cursor = if is_running { " " } else { "█" };

    if text.is_empty() {
        composer_lines.push(Line::from(vec![
            "❯ ".fg(prompt_color).bold(),
            block_cursor.fg(accent_cyan()),
        ]));
    } else {
        // Reconstruct the lines with the cursor inserted
        let mut reconstructed_lines = Vec::new();

        let before_last = before_lines.last().copied().unwrap_or("");
        let after_first = after_lines.first().copied().unwrap_or("");

        for line in before_lines
            .iter()
            .take(before_lines.len().saturating_sub(1))
        {
            reconstructed_lines.push(vec![Span::raw(*line)]);
        }

        let mut mid_line = vec![Span::raw(before_last)];
        let cursor_char = if after_first.is_empty() || after_first.starts_with('\n') {
            block_cursor.to_string()
        } else {
            after_first.chars().next().unwrap().to_string()
        };

        let after_rest = if !after_first.is_empty() && !after_first.starts_with('\n') {
            let ch_len = after_first.chars().next().unwrap().len_utf8();
            &after_first[ch_len..]
        } else {
            after_first
        };

        let cursor_style = ratatui::style::Style::default()
            .bg(accent_cyan())
            .fg(Color::Black);
        mid_line.push(Span::styled(cursor_char.clone(), cursor_style));
        mid_line.push(after_rest.into());

        if cursor_char == block_cursor && after_first.starts_with('\n') {
            mid_line.push(Span::raw("\n"));
        }

        reconstructed_lines.push(mid_line);

        for line in after_lines.iter().skip(1) {
            reconstructed_lines.push(vec![Span::raw(*line)]);
        }

        for (i, line_spans) in reconstructed_lines.into_iter().enumerate() {
            let prefix = if i == 0 { "❯ " } else { "  " };
            let mut final_spans = vec![prefix.fg(prompt_color).bold()];
            final_spans.extend(line_spans);
            composer_lines.push(Line::from(final_spans));
        }
    }

    let composer_widget = Paragraph::new(composer_lines).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(ratatui::style::Style::default().fg(Color::Indexed(236))),
    );

    f.render_widget(composer_widget, area);
    // Handle Overlays on top of the App
    match &state.overlay_state {
        OverlayState::None => {}
        OverlayState::CommandPalette {
            query,
            selected_idx,
        } => {
            render_command_palette(f, f.size(), query, *selected_idx);
        }
    }
}

pub fn render_command_palette(f: &mut Frame, area: Rect, query: &str, selected_idx: usize) {
    use ratatui::widgets::Clear;

    let palette_w = 60;
    let palette_h = 10;

    let x = area.x + (area.width.saturating_sub(palette_w)) / 2;
    let y = area.y + (area.height.saturating_sub(palette_h)) / 4; // Top 25% of screen

    let popup_area = Rect::new(x, y, palette_w, palette_h);

    // Clear underneath
    f.render_widget(Clear, popup_area);

    // Dynamic commands array
    let commands = crate::tui::state::get_palette_commands(query);

    let mut items = Vec::new();
    for (i, (cmd, desc)) in commands.iter().enumerate() {
        let style = if i == selected_idx {
            ratatui::style::Style::default()
                .bg(Color::Indexed(238))
                .fg(Color::White)
        } else {
            ratatui::style::Style::default().fg(Color::Indexed(245))
        };
        items.push(ListItem::new(format!("{cmd:<15} {desc}")).style(style));
    }

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Command Palette ({query}) "))
            .border_style(ratatui::style::Style::default().fg(accent_cyan())),
    );

    f.render_widget(list, popup_area);
}
