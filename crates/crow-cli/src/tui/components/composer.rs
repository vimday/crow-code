use super::super::component::{Component, TuiAction};
use super::super::state::AppState;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders},
    Frame,
};
use tui_textarea::TextArea;

/// Maximum number of command palette items visible at once.
const PALETTE_MAX_VISIBLE: usize = 8;

pub enum ActivePopup {
    None,
    CommandPalette {
        query: String,
        selected_idx: usize,
        /// Scroll offset: index of the first visible item.
        scroll_offset: usize,
        options: Vec<(String, String)>,
    },
}

pub struct ComposerComponent<'a> {
    pub textarea: TextArea<'a>,
    pub active_popup: ActivePopup,
}

impl<'a> Default for ComposerComponent<'a> {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a fresh textarea with standard styling.
/// Extracted to eliminate 4x duplication of textarea reset logic.
fn make_textarea<'a>() -> TextArea<'a> {
    let mut textarea = TextArea::default();
    textarea.set_block(Block::default().borders(Borders::NONE));
    textarea.set_cursor_line_style(ratatui::style::Style::new());
    // Hide the textarea's internal block cursor — we use frame.set_cursor()
    // to place a single terminal blinking cursor (Codex/Claude Code pattern).
    textarea.set_cursor_style(ratatui::style::Style::new());
    let placeholder = ratatui::style::Style::new().fg(ratatui::style::Color::DarkGray);
    textarea.set_placeholder_text("Ask Crow anything...");
    textarea.set_placeholder_style(placeholder);
    // NOTE: Do NOT call set_line_number_style() — it enables line numbers.
    // A chat input box should never show line numbers.
    textarea
}

impl<'a> ComposerComponent<'a> {
    pub fn new() -> Self {
        Self {
            textarea: make_textarea(),
            active_popup: ActivePopup::None,
        }
    }

    /// Reset textarea to a clean state. Used after submission.
    fn reset_textarea(&mut self) {
        self.textarea = make_textarea();
        self.active_popup = ActivePopup::None;
    }

    pub fn get_popup_height(&self, state: &AppState) -> u16 {
        if let crate::tui::state::ApprovalState::PendingCommand(..) = state.approval_state {
            return 5;
        }
        if let ActivePopup::CommandPalette { ref options, .. } = self.active_popup {
            // Visible items capped at PALETTE_MAX_VISIBLE, plus 2 for borders
            (options.len() as u16).min(PALETTE_MAX_VISIBLE as u16) + 2
        } else {
            0
        }
    }
}

impl<'a> Component for ComposerComponent<'a> {
    fn handle_event(&mut self, event: &Event, state: &mut AppState) -> Result<Option<TuiAction>> {
        // Handle bracketed paste events (Ctrl+V / terminal paste)
        if let Event::Paste(ref text) = event {
            for line in text.lines() {
                self.textarea.insert_str(line);
            }
            return Ok(None);
        }

        if let Event::Key(key) = event {
            // Check if we are in overlay mode
            if let ActivePopup::CommandPalette {
                query: _,
                ref mut selected_idx,
                ref mut scroll_offset,
                ref options,
            } = self.active_popup
            {
                if key.code == KeyCode::Esc {
                    self.active_popup = ActivePopup::None;
                    return Ok(None);
                }

                // Intercept navigation with auto-scroll
                if key.code == KeyCode::Up {
                    if *selected_idx > 0 {
                        *selected_idx -= 1;
                        // Scroll up if cursor moved above visible window
                        if *selected_idx < *scroll_offset {
                            *scroll_offset = *selected_idx;
                        }
                    }
                    return Ok(None);
                }
                if key.code == KeyCode::Down {
                    if *selected_idx < options.len().saturating_sub(1) {
                        *selected_idx += 1;
                        // Scroll down if cursor moved below visible window
                        if *selected_idx >= *scroll_offset + PALETTE_MAX_VISIBLE {
                            *scroll_offset = selected_idx.saturating_sub(PALETTE_MAX_VISIBLE - 1);
                        }
                    }
                    return Ok(None);
                }

                // Intercept autocomplete Enter
                if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
                    if let Some((cmd, _)) = options.get(*selected_idx) {
                        let text = cmd.clone();
                        self.reset_textarea();
                        return Ok(Some(TuiAction::SubmitCommand(text)));
                    }
                }
            }

            // ── Ctrl+U: clear current line (Unix convention) ──────────
            if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.reset_textarea();
                return Ok(None);
            }

            // ── Input history navigation: ↑/↓ when composer is empty ──
            if key.code == KeyCode::Up
                && self.textarea.lines().join("").trim().is_empty()
                && !state.input_history.is_empty()
            {
                let idx = state
                    .input_history_idx
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(state.input_history.len().saturating_sub(1));
                state.input_history_idx = Some(idx);
                self.reset_textarea();
                self.textarea.insert_str(&state.input_history[idx]);
                return Ok(None);
            }
            if key.code == KeyCode::Down && state.input_history_idx.is_some() {
                let idx = state.input_history_idx.unwrap_or(0) + 1;
                if idx < state.input_history.len() {
                    state.input_history_idx = Some(idx);
                    self.reset_textarea();
                    self.textarea.insert_str(&state.input_history[idx]);
                } else {
                    state.input_history_idx = None;
                    self.reset_textarea();
                }
                return Ok(None);
            }

            // Normal textarea handling — Enter submits (Shift+Enter = newline)
            if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
                let lines = self.textarea.lines().to_vec();
                let text = lines.join("\n");
                self.reset_textarea();
                return Ok(Some(TuiAction::SubmitCommand(text)));
            }

            self.textarea.input(*key);

            // Post-mutation text analysis for the Popup logic
            let lines = self.textarea.lines();
            if lines.len() == 1 && (lines[0].starts_with('/') || lines[0].starts_with('!')) {
                let query = lines[0].to_string();
                let options = crow_commands::get_palette_commands(&query);

                if !options.is_empty() {
                    self.active_popup = ActivePopup::CommandPalette {
                        query,
                        selected_idx: 0,
                        scroll_offset: 0,
                        options,
                    };
                } else {
                    self.active_popup = ActivePopup::None;
                }
            } else {
                self.active_popup = ActivePopup::None;
            }
        }
        Ok(None)
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        // If there is a pending command approval, render the security prompt
        if let crate::tui::state::ApprovalState::PendingCommand(ref cmd, selected_idx) =
            state.approval_state
        {
            render_approval_popup(frame, area, cmd, selected_idx);
            return;
        }

        // Ensure block remains NONE
        self.textarea
            .set_block(Block::default().borders(Borders::NONE));

        let popup_h = self.get_popup_height(state);
        let split = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Length(popup_h),
                ratatui::layout::Constraint::Min(0),
            ])
            .split(area);

        let popup_area = split[0];
        let composer_area = split[1];

        let composer_split = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([
                ratatui::layout::Constraint::Length(2),
                ratatui::layout::Constraint::Min(0),
            ])
            .split(composer_area);

        use crate::tui::theme::{chars, spinner_char, Styles};

        let prompt_text = if state.is_task_running() {
            format!("{} ", spinner_char(state.spinner_idx))
        } else {
            format!("{} ", chars::INPUT_PROMPT)
        };

        let prompt_style = if state.is_task_running() {
            Styles::spinner()
        } else {
            Styles::input_prompt()
        };

        let prompt_widget = ratatui::widgets::Paragraph::new(prompt_text).style(prompt_style);

        frame.render_widget(prompt_widget, composer_split[0]);
        frame.render_widget(self.textarea.widget(), composer_split[1]);

        // Always set the terminal cursor at the text insertion point so the
        // user can see where they are typing (Codex/Claude Code UX pattern).
        // Without this, the cursor is invisible and users must "blind-type."
        if state.focus == crate::tui::state::Focus::Composer {
            let (cursor_row, cursor_col) = self.textarea.cursor();
            let x = composer_split[1].x + cursor_col as u16;
            let y = composer_split[1].y + cursor_row as u16;
            // Clamp to the composer area to prevent cursor from escaping
            let clamped_x = x.min(composer_split[1].right().saturating_sub(1));
            let clamped_y = y.min(composer_split[1].bottom().saturating_sub(1));
            frame.set_cursor(clamped_x, clamped_y);
        }

        // Draw the floating popup if active
        if let ActivePopup::CommandPalette {
            query: _,
            selected_idx,
            scroll_offset,
            ref options,
        } = self.active_popup
        {
            if popup_h > 0 {
                use ratatui::style::{Color, Stylize};
                use ratatui::widgets::{Block, Borders, Clear, List, ListItem};

                frame.render_widget(Clear, popup_area); // Erase underlying content

                // Only render the visible window of items
                let visible_end = (scroll_offset + PALETTE_MAX_VISIBLE).min(options.len());
                let has_more_above = scroll_offset > 0;
                let has_more_below = visible_end < options.len();

                let list_items: Vec<ListItem> = options[scroll_offset..visible_end]
                    .iter()
                    .enumerate()
                    .map(|(vis_i, (cmd, desc))| {
                        let abs_i = vis_i + scroll_offset;
                        let content = format!(" {cmd:18} {desc}");
                        if abs_i == selected_idx {
                            ListItem::new(content).style(
                                ratatui::style::Style::new()
                                    .bg(Color::Cyan)
                                    .fg(Color::Black)
                                    .bold(),
                            )
                        } else {
                            ListItem::new(content)
                        }
                    })
                    .collect();

                // Build title with scroll indicators
                let title = if has_more_above && has_more_below {
                    " Commands ▲▼ "
                } else if has_more_above {
                    " Commands ▲ "
                } else if has_more_below {
                    " Commands ▼ "
                } else {
                    " Commands "
                };

                let popup_list = List::new(list_items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(ratatui::style::Style::new().fg(Color::Cyan))
                        .title(title),
                );

                // Dynamic width: 55% of terminal width, clamped to [40, 60]
                let popup_width = (area.width * 55 / 100).clamp(40, 60);

                // Render List on the left side of the popup area
                let popup_horiz = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Horizontal)
                    .constraints([
                        ratatui::layout::Constraint::Length(popup_width),
                        ratatui::layout::Constraint::Min(0),
                    ])
                    .split(popup_area);

                frame.render_widget(popup_list, popup_horiz[0]);
            }
        }
    }
}

// ── Extracted approval popup renderer ─────────────────────────────────

/// Render the security approval popup. Extracted from inline render() to
/// reduce complexity and enable dynamic sizing.
fn render_approval_popup(frame: &mut Frame, area: Rect, cmd: &str, selected_idx: usize) {
    use ratatui::style::Stylize;
    use ratatui::text::Line;
    use ratatui::widgets::{List, ListItem, Paragraph};

    let composer_lines = vec![
        Line::from(vec!["⚠️  Security Approval Required".red().bold()]),
        Line::from(vec!["Command: ".dark_gray(), cmd.to_string().into()]),
        Line::from(vec![
            "  (y=Allow  a=Always  n=Reject  Esc=Cancel)".dark_gray()
        ]),
    ];

    let composer_widget =
        Paragraph::new(composer_lines).block(Block::default().borders(Borders::NONE));
    frame.render_widget(composer_widget, area);

    // Render floating interaction popup — dynamically sized to terminal width
    let options = [
        "[✓] Allow Once",
        "[★] Allow Always (Whitelist)",
        "[X] Reject",
    ];
    let list_items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, &opt)| {
            if i == selected_idx {
                ListItem::new(opt).style(
                    ratatui::style::Style::new()
                        .bg(ratatui::style::Color::LightRed)
                        .fg(ratatui::style::Color::Black)
                        .bold(),
                )
            } else {
                ListItem::new(opt)
            }
        })
        .collect();

    let popup_list = List::new(list_items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::new().fg(ratatui::style::Color::LightRed))
            .title(" Action "),
    );

    // Dynamic sizing: cap popup width to terminal width - 12, minimum 30
    let popup_width = area.width.saturating_sub(12).clamp(30, 40);
    let popup_area = Rect {
        x: area.x.saturating_add(6),
        y: area.y.saturating_sub(5),
        width: popup_width,
        height: 5,
    };
    frame.render_widget(ratatui::widgets::Clear, popup_area);
    frame.render_widget(popup_list, popup_area);
}
