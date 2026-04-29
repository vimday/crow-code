use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Styled, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use super::state::AppState;
use super::theme::{chars, colors, Styles};

// Re-export MarkdownStreamState for state.rs
pub use crate::render::MarkdownStreamState;


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
        .constraints([Constraint::Percentage(78), Constraint::Percentage(22)])
        .split(size);

    let main_area = main_split[0];
    let side_area = main_split[1];

    let status_lines = if state.is_streaming || state.active_action.is_some() || state.status_indicator.is_some() {
        if state.status_indicator.as_ref().is_some_and(|i| i.details.is_some()) {
            4 // Status header + 3 details lines max
        } else {
            1
        }
    } else {
        1 // just the right side context
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),                 // Conversation pane
            Constraint::Length(swarm_lines),    // Swarm bar
            Constraint::Length(status_lines),   // Status indicator
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

    use ratatui::widgets::Widget;
    crate::tui::components::status::StatusIndicatorWidget::new(state).render(chunks[2], f.buffer_mut());
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
    use ratatui::style::{Color, Style};
    use ratatui::widgets::{Block, Borders, Paragraph};

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(Color::DarkGray));

    let mut lines = Vec::new();
    lines.push(Line::from(""));

    lines.push(Line::from(vec![
        format!(" {} ", chars::CODE_TOP_LEFT).set_style(Styles::user_header()),
        "ENVIRONMENT".set_style(Styles::evidence()),
    ]));

    let path = if state.workspace_name.is_empty() {
        "memfs"
    } else {
        &state.workspace_name
    };
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
        },
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

// ── Conversation Pane (trait-based rendering) ────────────────────────────────

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

    // 1. Active streaming cell (trait-based)
    if let Some(ref cell) = state.active_cell {
        if to_take > 0 {
            let lines = cell.display_lines(area.width);
            for item in lines.into_iter().rev() {
                push_item!(ListItem::new(item));
            }
        }
    }

    // 2. Iterate history backwards using HistoryCell trait dispatch
    for cell in state.history.iter().rev() {
        if to_take == 0 {
            break;
        }

        let lines = cell.display_lines(area.width);

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

    let p =
        Paragraph::new(Line::from(spans)).style(ratatui::style::Style::new().bg(colors::border()));
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
            Line::from(vec!["  ↑/↓".bold().dim(), " input history".dim()]),
            Line::from("  ? again to dismiss".dim()),
        ]
    } else if state
        .quit_hint_until
        .is_some_and(|t| std::time::Instant::now() < t)
    {
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

    let p = Paragraph::new(lines).style(Style::new());
    f.render_widget(p, area);
}

// ── Composer ─────────────────────────────────────────────────────────────────
// Codex pattern: top border only, left gutter aligned, `❯ ` prompt.
