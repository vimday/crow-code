use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Stylize, Styled};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use super::state::{AppState, CellKind};
use super::theme::{chars, colors, Styles};

/// Left gutter width matching Codex's LIVE_PREFIX_COLS.
const GUTTER: &str = "  ";

// ── Color aliases from theme (backwards compat for cells) ────────────────────
fn dim_gray() -> Color {
    colors::text_muted()
}
fn accent_cyan() -> Color {
    colors::accent_system()
}
fn accent_red() -> Color {
    colors::accent_error()
}

// ── Spinner frames from theme ────────────────────────────────────────────────
const SPINNER: &[&str] = chars::SPINNER;

pub fn render_app(
    f: &mut Frame,
    state: &mut AppState,
    composer_comp: &mut crate::tui::components::composer::ComposerComponent,
    history_comp: &mut crate::tui::components::history::HistoryComponent,
) {
    let size = f.size();

    let composer_lines = if matches!(
        state.approval_state,
        crate::tui::state::ApprovalState::PendingCommand(..)
    ) {
        3
    } else {
        // Assume text area height defaults to 5 for now
        5
    };

    let swarm_lines = if state.active_swarms.is_empty() { 0 } else { 1 };
    let popup_lines = composer_comp.get_popup_height(state);

    // Determine footer hint height (Codex pattern: contextual keyboard hints)
    let footer_lines: u16 = if state.show_shortcuts_overlay {
        7 // shortcut overlay (multi-line)
    } else {
        1 // quit hint / interrupt hint / shortcut hint
    };

    let main_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(78),
            Constraint::Percentage(22),
        ])
        .split(size);

    let main_area = main_split[0];
    let side_area = main_split[1];

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),                 // Conversation pane
            Constraint::Length(swarm_lines),    // Swarm bar
            Constraint::Length(1),              // Status bar
            Constraint::Length(footer_lines),   // Footer hints
            Constraint::Length(popup_lines),    // Dynamic Command Palette Popup
            Constraint::Length(composer_lines), // Composer
        ])
        .split(main_area);

    use crate::tui::component::Component;

    history_comp.render(f, chunks[0], state);

    if swarm_lines > 0 {
        render_swarm_bar(f, state, chunks[1]);
    }

    render_status_bar(f, state, chunks[2]);
    render_footer_hints(f, state, chunks[3]);

    // Group the bottom areas for passing to composer
    let compound_composer_rect = ratatui::layout::Rect {
        x: chunks[4].x,
        y: chunks[4].y,
        width: chunks[4].width,
        height: chunks[4].height + chunks[5].height,
    };
    composer_comp.render(f, compound_composer_rect, state);
    
    // Render side context dashboard
    render_side_context(f, state, side_area);
}

// ── Side Context Dashboard ───────────────────────────────────────────────────

fn render_side_context(f: &mut Frame, state: &AppState, area: Rect) {
    use ratatui::widgets::{Block, Borders, Paragraph};
    use ratatui::style::{Style, Color};
    
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(Color::DarkGray));
        
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    
    lines.push(Line::from(vec![
        format!(" {} ", chars::CODE_TOP_LEFT).set_style(Styles::user_header()),
        "ENVIRONMENT".set_style(Styles::evidence()),
    ]));
    
    let path = if state.workspace_name.is_empty() { "memfs" } else { &state.workspace_name };
    lines.push(Line::from(vec![
        "    Path:   ".set_style(Styles::evidence()),
        path.set_style(Styles::code_block()),
    ]));
    
    lines.push(Line::from(vec![
        "    Branch: ".set_style(Styles::evidence()),
        state.git_branch.as_str().set_style(Styles::code_block()),
        if state.is_dirty {
            " *".set_style(Styles::error())
        } else {
            "".set_style(Styles::evidence())
        }
    ]));
    
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        format!(" {} ", chars::CODE_TOP_LEFT).set_style(Styles::user_header()),
        "AGENT CONTEXT".set_style(Styles::evidence()),
    ]));
    
    let mode_str = format!("{:?}", state.view_mode);
    lines.push(Line::from(vec![
        "    Auth:   ".set_style(Styles::evidence()),
        mode_str.set_style(Styles::success()),
    ]));
    
    lines.push(Line::from(vec![
        "    Write:  ".set_style(Styles::evidence()),
        state.write_mode.as_str().set_style(Styles::warning()),
    ]));
        
    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}

// ── Conversation Pane ────────────────────────────────────────────────────────

pub trait HistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;
}

struct UserCell<'a>(&'a str);
impl<'a> HistoryCell for UserCell<'a> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(4).max(1) as usize;
        let wrapped = textwrap::wrap(self.0, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if wrapped.len() == 1 {
                format!("{} ", chars::USER_BAR).set_style(Styles::user_header())
            } else if i == 0 {
                format!("{} ", chars::CODE_TOP_LEFT).set_style(Styles::user_header())
            } else if i == wrapped.len() - 1 {
                format!("{} ", chars::CODE_BOTTOM_LEFT).set_style(Styles::user_header())
            } else {
                format!("{} ", chars::USER_BAR).set_style(Styles::user_header())
            };
            lines.push(Line::from(vec![
                prefix,
                line.to_string().set_style(Styles::user_content()),
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
            let prefix = if md_lines.len() == 1 {
                format!("{} ", chars::USER_BAR).set_style(Styles::assistant_content())
            } else if i == 0 {
                format!("{} ", chars::CODE_TOP_LEFT).set_style(Styles::assistant_content())
            } else if i == md_lines.len() - 1 {
                format!("{} ", chars::CODE_BOTTOM_LEFT).set_style(Styles::assistant_content())
            } else {
                format!("{} ", chars::USER_BAR).set_style(Styles::assistant_content())
            };
            
            let mut new_spans = vec![prefix];
            for span in line.spans.iter() {
                new_spans.push(span.clone());
            }
            out.push(Line::from(new_spans));
        }
        if out.is_empty() {
            // Fallback for empty content
            out.push(Line::from(vec![
                format!("{} ", chars::USER_BAR).set_style(Styles::assistant_content()),
                self.0.to_string().set_style(Styles::assistant_content()),
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
                format!("{GUTTER}{} ", chars::BULLET)
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.set_style(Styles::evidence()),
                line.to_string().set_style(Styles::evidence()),
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
                format!("{GUTTER}▶ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.set_style(Styles::success()),
                line.to_string().set_style(Styles::success()),
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
                prefix.set_style(Styles::tool_header()),
                line.to_string().set_style(Styles::tool_header()),
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
                format!("{GUTTER}{} ", chars::BULLET)
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.set_style(Styles::evidence()),
                line.to_string().set_style(Styles::evidence()),
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
                prefix.set_style(Styles::error()),
                line.to_string().set_style(Styles::error()),
            ]));
        }
        lines
    }
}

struct DebateCell<'a>(&'a str);
impl<'a> HistoryCell for DebateCell<'a> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(self.0, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}⚖ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.fg(Color::Magenta),
                line.to_string().fg(Color::Magenta),
            ]));
        }
        lines
    }
}

pub fn render_history_pane(f: &mut Frame, state: &AppState, area: Rect) {
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

    // 1. Active spinner is at the very bottom (with shimmer animation)
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
            // Use shimmer animation for the first line (active action)
            if i == 0 {
                let mut spans = vec![prefix.fg(accent_cyan())];
                spans.extend(crate::tui::shimmer::shimmer_spans(line));
                lines.push(Line::from(spans));
            } else {
                lines.push(Line::from(vec![
                    prefix.fg(accent_cyan()),
                    line.to_string().fg(accent_cyan()),
                ]));
            }
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
            CellKind::Debate => Box::new(DebateCell(&cell.payload)),
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
        .style(ratatui::style::Style::new().bg(colors::border()));
    f.render_widget(p, area);
}

// ── Footer Hints (Codex pattern: contextual keyboard affordances) ────────────

fn render_footer_hints(f: &mut Frame, state: &AppState, area: Rect) {
    if area.width < 4 || area.height == 0 {
        return;
    }

    use ratatui::style::{Style, Stylize};

    let lines = if state.show_shortcuts_overlay {
        // Full shortcut overlay (Codex `?` key pattern)
        vec![
            Line::from(vec![
                "  enter".bold().dim(),
                " submit".dim(),
                "      ".into(),
                "esc".bold().dim(),
                " interrupt".dim(),
            ]),
            Line::from(vec![
                "  shift+enter".bold().dim(),
                " newline".dim(),
                "  ".into(),
                "ctrl+c".bold().dim(),
                " quit (×2)".dim(),
            ]),
            Line::from(vec![
                "  /".bold().dim(),
                " commands".dim(),
                "        ".into(),
                "ctrl+d".bold().dim(),
                " quit now".dim(),
            ]),
            Line::from(vec![
                "  !".bold().dim(),
                " shell cmd".dim(),
                "       ".into(),
                "tab".bold().dim(),
                " switch focus".dim(),
            ]),
            Line::from(vec![
                "  pgup/pgdn".bold().dim(),
                " scroll".dim(),
                "   ".into(),
                "ctrl+u".bold().dim(),
                " clear input".dim(),
            ]),
            Line::from(vec![
                "  ↑/↓".bold().dim(),
                " input history".dim(),
            ]),
            Line::from("  ? again to dismiss".dim()),
        ]
    } else if state.quit_hint_until.is_some_and(|t| std::time::Instant::now() < t) {
        vec![Line::from(vec![
            "  ".into(),
            "ctrl+c".bold().fg(colors::accent_warning()),
            " again to quit".fg(colors::accent_warning()),
        ])]
    } else if state.is_task_running() {
        vec![Line::from(vec![
            "  ".into(),
            "esc".bold().dim(),
            " to interrupt".dim(),
        ])]
    } else {
        vec![Line::from(vec![
            "  ".into(),
            "?".bold().dim(),
            " for shortcuts".dim(),
        ])]
    };

    let p = Paragraph::new(lines)
        .style(Style::new());
    f.render_widget(p, area);
}

// ── Status Bar ───────────────────────────────────────────────────────────────
// Codex pattern: left-side hints, right-side context joined by ` · `,
// connected by `─` fill.

fn render_status_bar(f: &mut Frame, state: &AppState, area: Rect) {
    if area.width < 4 {
        return;
    }

    // ── LEFT: Mode + Git + Streaming indicator ──────────────────────
    let left = if state.is_streaming {
        let spinner = chars::SPINNER[state.spinner_idx % chars::SPINNER.len()];
        let elapsed = state
            .streaming_start_time
            .map(|t| {
                let secs = t.elapsed().as_secs();
                if secs < 60 {
                    format!("{secs}s")
                } else {
                    format!("{}m{}s", secs / 60, secs % 60)
                }
            })
            .unwrap_or_default();
        let tokens = state.streaming_token_estimate;
        let token_display = if tokens < 1000.0 {
            format!("{tokens:.0}")
        } else {
            format!("{:.1}k", tokens / 1000.0)
        };
        format!(" {spinner} {token_display} tok · {elapsed} ")
    } else if state.is_task_running() {
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

    // ── CENTER: Timed status messages or active action ──────────────
    let center = if let Some(ref msg) = state.status_message {
        let color = match msg.level {
            crate::tui::state::StatusLevel::Info => accent_cyan(),
            crate::tui::state::StatusLevel::Warn => colors::accent_warning(),
            crate::tui::state::StatusLevel::Error => colors::accent_error(),
            crate::tui::state::StatusLevel::Tip => dim_gray(),
        };
        (msg.content.clone(), color)
    } else if let Some(ref action) = state.active_action {
        let action_display = if action.len() > 30 {
            format!("{}…", &action[..29])
        } else {
            action.clone()
        };
        (action_display, accent_cyan())
    } else {
        (String::new(), dim_gray())
    };

    // ── RIGHT: Model · Context usage (Yomi pattern) ────────────────
    let mut right_parts: Vec<String> = Vec::new();
    right_parts.push(state.model_info.clone());

    // Context window usage (color-coded like Yomi)
    #[allow(clippy::cast_precision_loss)]
    if let Some((tokens, context_window)) = state.ctx_usage {
        if context_window > 0 {
            let pct = tokens as f32 / context_window as f32;
            let cw_k = context_window / 1000;
            right_parts.push(format!("{:.1}% ({cw_k}K)", pct * 100.0));
        }
    }

    right_parts.push(format!("{:?}", state.view_mode));
    let right = format!(" {} ", right_parts.join(" · "));

    let left_span = Line::from(vec![
        if state.is_streaming {
            left.fg(accent_cyan())
        } else {
            left.fg(dim_gray())
        },
        git_info.fg(risk_color),
    ]);

    let left_w = left_span.width().min(area.width as usize);
    let right_w = right.chars().count().min(area.width as usize);

    let status_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_w as u16 + 2),
            Constraint::Min(0),
            Constraint::Length(right_w as u16),
        ])
        .split(area);

    let left_widget = Paragraph::new(left_span);

    // Context window usage color
    #[allow(clippy::cast_precision_loss)]
    let right_color = state
        .ctx_usage
        .map(|(tokens, cw)| {
            if cw == 0 {
                return dim_gray();
            }
            let pct = tokens as f32 / cw as f32;
            if pct >= 0.9 {
                accent_red()
            } else if pct >= 0.7 {
                colors::accent_warning()
            } else {
                dim_gray()
            }
        })
        .unwrap_or(dim_gray());
    let right_widget = Paragraph::new(right.fg(right_color));

    // Center section: status message or divider fill
    let mid_w = status_chunks[1].width as usize;
    let (center_text, center_color) = center;
    let mid_widget = if !center_text.is_empty() && center_text.len() <= mid_w {
        let pad_left = (mid_w.saturating_sub(center_text.len())) / 2;
        let pad_right = mid_w.saturating_sub(center_text.len()).saturating_sub(pad_left);
        Paragraph::new(Line::from(vec![
            "─".repeat(pad_left).fg(colors::divider()),
            center_text.fg(center_color),
            "─".repeat(pad_right).fg(colors::divider()),
        ]))
    } else {
        let mid_fill = "─".repeat(mid_w);
        Paragraph::new(mid_fill.fg(colors::divider()))
    };

    f.render_widget(left_widget, status_chunks[0]);
    f.render_widget(mid_widget, status_chunks[1]);
    f.render_widget(right_widget, status_chunks[2]);
}

// ── Composer ─────────────────────────────────────────────────────────────────
// Codex pattern: top border only, left gutter aligned, `❯ ` prompt.
