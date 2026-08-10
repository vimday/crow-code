use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Styled, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use super::state::AppState;
use super::theme::{chars, colors, Styles};
use crate::tui::text_utils::truncate_to_width;

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
    let footer_lines: u16 = crate::tui::footer::footer_height(state);

    let frame_layout = crate::tui::layout::compute_frame_layout(size);
    let main_area = frame_layout.main;
    let side_area = frame_layout.cockpit;

    let status_lines = if let Some(ind) = state.status_indicator.as_ref() {
        if ind.details.is_some() || ind.progress_pct.is_some() {
            2
        } else {
            1
        }
    } else {
        1
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
    crate::tui::components::status::StatusIndicatorWidget::new(state)
        .render(chunks[2], f.buffer_mut());
    render_footer_hints(f, state, chunks[3]);

    // Group the bottom areas for passing to composer
    let compound_composer_rect = ratatui::layout::Rect {
        x: chunks[4].x,
        y: chunks[4].y,
        width: chunks[4].width,
        height: chunks[4].height + chunks[5].height,
    };
    composer_comp.render(f, compound_composer_rect, state);

    if let Some(side_area) = side_area {
        render_side_context(f, state, side_area);
    }
}

// ── Side Context Dashboard ───────────────────────────────────────────────────

fn render_side_context(f: &mut Frame, state: &AppState, area: Rect) {
    use ratatui::style::Style;
    use ratatui::widgets::{Block, Borders, Paragraph};

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(colors::divider()));

    let narrow = area.width < 32;

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        "  CROW".set_style(Styles::user_header()),
        " / ".set_style(Styles::evidence()),
        state
            .workspace_name
            .as_str()
            .set_style(Styles::code_block()),
    ]));

    let dirty = if state.is_dirty { "*" } else { "" };
    let branch_line = if narrow {
        vec![
            "  ".into(),
            state.git_branch.as_str().set_style(Styles::success()),
            dirty.set_style(Styles::error()),
            " · ".set_style(Styles::evidence()),
            state.write_mode.as_str().set_style(Styles::warning()),
        ]
    } else {
        vec![
            "  ".into(),
            state.git_branch.as_str().set_style(Styles::success()),
            dirty.set_style(Styles::error()),
            " · ".set_style(Styles::evidence()),
            format!("{:?}", state.view_mode).set_style(Styles::evidence()),
            " · ".set_style(Styles::evidence()),
            state.write_mode.as_str().set_style(Styles::warning()),
        ]
    };
    lines.push(Line::from(branch_line));

    if !state.auto_run.agents.is_empty() {
        if !narrow {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                "  AUTO".set_style(Styles::user_header()),
                " swarm".set_style(Styles::evidence()),
            ]));
        }
        for line in crate::tui::agent_status_feed::render_cockpit_lines(
            &state.auto_run,
            area.width.saturating_sub(2),
        ) {
            lines.push(line);
        }
    }

    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}

// ── Conversation Pane (trait-based rendering) ────────────────────────────────

pub fn history_viewport_lines(
    state: &AppState,
    width: u16,
    viewport_height: usize,
) -> Vec<Line<'static>> {
    if viewport_height == 0 {
        return Vec::new();
    }

    let mut all_lines = Vec::new();
    for cell in &state.history {
        all_lines.extend(cell.display_lines(width));
    }
    if let Some(ref cell) = state.active_cell {
        all_lines.extend(cell.display_lines(width));
    }

    let total = all_lines.len();
    let end = total.saturating_sub(state.scroll_offset).min(total);
    let start = end.saturating_sub(viewport_height);
    let mut visible = all_lines[start..end].to_vec();

    if visible.len() < viewport_height {
        let mut padded = vec![Line::from(""); viewport_height - visible.len()];
        padded.append(&mut visible);
        padded
    } else {
        visible
    }
}

pub fn render_history_pane(f: &mut Frame, state: &AppState, area: Rect) {
    let viewport = area.height as usize;
    if viewport == 0 {
        return;
    }

    let items = history_viewport_lines(state, area.width, viewport)
        .into_iter()
        .map(ListItem::new)
        .collect::<Vec<_>>();

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
        let display_task = truncate_to_width(task, 30);
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

    use ratatui::style::Style;

    let p = Paragraph::new(crate::tui::footer::footer_hint_lines(state)).style(Style::new());
    f.render_widget(p, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::history_cell::LogCell;

    fn text(lines: Vec<Line<'static>>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn history_viewport_scrolls_by_rendered_rows_not_cells() {
        let mut state = AppState::new("model".into(), "write".into(), "workspace".into());
        state.history.push(Box::new(LogCell {
            payload: "alpha beta gamma delta".into(),
        }));
        state.history.push(Box::new(LogCell {
            payload: "tail".into(),
        }));
        state.scroll_offset = 1;

        let lines = text(history_viewport_lines(&state, 16, 2));

        assert_eq!(lines.len(), 2);
        assert!(lines.join("\n").contains("gamma"));
        assert!(lines.join("\n").contains("delta"));
        assert!(!lines.join("\n").contains("tail"));
    }

    #[test]
    fn history_viewport_pads_when_content_is_short() {
        let mut state = AppState::new("model".into(), "write".into(), "workspace".into());
        state.history.push(Box::new(LogCell {
            payload: "only".into(),
        }));

        let lines = text(history_viewport_lines(&state, 40, 3));

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "");
        assert_eq!(lines[1], "");
        assert!(lines[2].contains("only"));
    }
}
