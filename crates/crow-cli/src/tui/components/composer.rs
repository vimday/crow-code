use super::super::component::{Component, TuiAction};
use super::super::state::AppState;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::Style,
    widgets::{Block, Borders},
    Frame,
};
use tui_textarea::TextArea;

pub enum ActivePopup {
    None,
    CommandPalette { query: String, selected_idx: usize, options: Vec<(String, String)> },
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

impl<'a> ComposerComponent<'a> {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(Block::default().borders(Borders::NONE));
        textarea.set_cursor_line_style(Style::default());
        let placeholder = Style::default().fg(ratatui::style::Color::DarkGray);
        textarea.set_placeholder_text("Ask Crow anything...");
        textarea.set_placeholder_style(placeholder);
        
        // Remove line numbers
        textarea.set_line_number_style(Style::default().fg(ratatui::style::Color::DarkGray));
        Self { textarea, active_popup: ActivePopup::None }
    }

    pub fn get_popup_height(&self, state: &AppState) -> u16 {
        if let crate::tui::state::ApprovalState::PendingCommand(..) = state.approval_state {
            return 5;
        }
        if let ActivePopup::CommandPalette { ref options, .. } = self.active_popup {
            // max 5 options height, plus 2 for borders
            (options.len() as u16).min(5) + 2
        } else {
            0
        }
    }
}


impl<'a> Component for ComposerComponent<'a> {
    fn handle_event(&mut self, event: &Event, _state: &mut AppState) -> Result<Option<TuiAction>> {
        if let Event::Key(key) = event {
            // Check if we are in overlay mode
            if let ActivePopup::CommandPalette { query: _, ref mut selected_idx, ref options } = self.active_popup {
                if key.code == KeyCode::Esc {
                    self.active_popup = ActivePopup::None;
                    return Ok(None);
                }
                
                // Intercept navigation
                if key.code == KeyCode::Up {
                    if *selected_idx > 0 {
                        *selected_idx -= 1;
                    }
                    return Ok(None);
                }
                if key.code == KeyCode::Down {
                    if *selected_idx < options.len().saturating_sub(1) {
                        *selected_idx += 1;
                    }
                    return Ok(None);
                }
                
                // Intercept autocomplete Enter
                if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
                    if let Some((cmd, _)) = options.get(*selected_idx) {
                        let text = cmd.clone();
                        // Reset and submit
                        self.active_popup = ActivePopup::None;
                        self.textarea = TextArea::default();
                        self.textarea.set_block(Block::default().borders(Borders::NONE));
                        self.textarea.set_cursor_line_style(Style::default());
                        let placeholder = Style::default().fg(ratatui::style::Color::DarkGray);
                        self.textarea.set_placeholder_text("Ask Crow anything...");
                        self.textarea.set_placeholder_style(placeholder);
                        self.textarea.set_line_number_style(Style::default().fg(ratatui::style::Color::DarkGray));
                        return Ok(Some(TuiAction::SubmitCommand(text)));
                    }
                }
            }

            // Normal textarea handling (arrows, typings, etc.)
            // If they press Enter here (no popup), submit it
            if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
                let lines = self.textarea.lines().to_vec();
                let text = lines.join("\n");
                
                // Clear the textarea after passing content up
                self.textarea = TextArea::default();
                self.textarea.set_block(Block::default().borders(Borders::NONE));
                self.textarea.set_cursor_line_style(Style::default());
                let placeholder = Style::default().fg(ratatui::style::Color::DarkGray);
                self.textarea.set_placeholder_text("Ask Crow anything...");
                self.textarea.set_placeholder_style(placeholder);
                self.textarea.set_line_number_style(Style::default().fg(ratatui::style::Color::DarkGray));
                
                self.active_popup = ActivePopup::None; // Explicit reset
                
                return Ok(Some(TuiAction::SubmitCommand(text)));
            }

            self.textarea.input(*key);
            
            // Post-mutation text analysis for the Popup logic
            let lines = self.textarea.lines();
            if lines.len() == 1 && (lines[0].starts_with('/') || lines[0].starts_with('!')) {
                let query = lines[0].to_string();
                let options = crate::tui::state::get_palette_commands(&query);
                
                if !options.is_empty() {
                    self.active_popup = ActivePopup::CommandPalette { query, selected_idx: 0, options };
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
        // If there is a pending command approval, render the security prompt instead
        if let crate::tui::state::ApprovalState::PendingCommand(ref cmd, selected_idx) = state.approval_state {
            use ratatui::text::{Line, Span};
            use ratatui::widgets::{Paragraph, List, ListItem};
            use ratatui::style::Stylize;
            
            let mut composer_lines = Vec::new();
            composer_lines.push(Line::from(vec![Span::styled(
                "⚠️  Security Approval Required",
                ratatui::style::Style::default().fg(ratatui::style::Color::Red).bold(),
            )]));
            composer_lines.push(Line::from(vec![
                "Command: ".dark_gray(),
                cmd.clone().into(),
            ]));

            let composer_widget = Paragraph::new(composer_lines).block(
                Block::default().borders(Borders::NONE)
            );
            frame.render_widget(composer_widget, area);

            // Render floating interaction popup
            let options = ["[✓] Allow Once",
                "[★] Allow Always (Whitelist)",
                "[X] Reject"];
            let list_items: Vec<ListItem> = options
                .iter()
                .enumerate()
                .map(|(i, &opt)| {
                    if i == selected_idx {
                        ListItem::new(opt).style(Style::default().bg(ratatui::style::Color::LightRed).fg(ratatui::style::Color::Black).bold())
                    } else {
                        ListItem::new(opt)
                    }
                })
                .collect();
                
            let popup_list = List::new(list_items)
                .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(ratatui::style::Color::LightRed)).title(" Action "));
            
            let popup_area = Rect {
                x: area.x.saturating_add(6),
                y: area.y.saturating_sub(5),
                width: 35,
                height: 5,
            };
            frame.render_widget(ratatui::widgets::Clear, popup_area);
            frame.render_widget(popup_list, popup_area);
            
            return;
        }

        // Ensure block remains NONE
        self.textarea.set_block(Block::default().borders(Borders::NONE));
        
        let popup_h = self.get_popup_height(state);
        let split = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([ratatui::layout::Constraint::Length(popup_h), ratatui::layout::Constraint::Min(0)])
            .split(area);
            
        let popup_area = split[0];
        let composer_area = split[1];
        
        let composer_split = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([ratatui::layout::Constraint::Length(2), ratatui::layout::Constraint::Min(0)])
            .split(composer_area);
            
        let prompt_color = if state.is_task_running() { ratatui::style::Color::DarkGray } else { ratatui::style::Color::Cyan };
        let prompt_widget = ratatui::widgets::Paragraph::new("❯ ").style(ratatui::style::Style::default().fg(prompt_color).add_modifier(ratatui::style::Modifier::BOLD));
        
        frame.render_widget(prompt_widget, composer_split[0]);
        frame.render_widget(self.textarea.widget(), composer_split[1]);
        
        // Draw the floating popup if active
        if let ActivePopup::CommandPalette { query: _, selected_idx, ref options } = self.active_popup {
            if popup_h > 0 {
                use ratatui::widgets::{Clear, List, ListItem, Borders, Block};
                use ratatui::style::{Style, Color, Stylize};
                
                frame.render_widget(Clear, popup_area); // Erase underlying content
                
                let list_items: Vec<ListItem> = options
                    .iter()
                    .enumerate()
                    .map(|(i, (cmd, desc))| {
                        let content = format!(" {cmd:15} |  {desc} ");
                        if i == selected_idx {
                            ListItem::new(content)
                                .style(Style::default().bg(Color::Cyan).fg(Color::Black).bold())
                        } else {
                            ListItem::new(content)
                        }
                    })
                    .collect();
                    
                let popup_list = List::new(list_items)
                    .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)).title(" Commands "));
                    
                // Render List on the left side of the popup area
                let popup_horiz = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Horizontal)
                    .constraints([ratatui::layout::Constraint::Length(30), ratatui::layout::Constraint::Min(0)])
                    .split(popup_area);
                    
                frame.render_widget(popup_list, popup_horiz[0]);
            }
        }
    }
}
