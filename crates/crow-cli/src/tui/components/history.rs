use super::super::component::{Component, TuiAction};

use super::super::state::AppState;
use anyhow::Result;
use crossterm::event::{Event, KeyCode};
use ratatui::{layout::Rect, Frame};

pub struct HistoryComponent {
    /// Whether auto-scroll to bottom is active.
    /// Disabled when user scrolls up, re-enabled on new streaming content or Home.
    auto_scroll: bool,
}

impl Default for HistoryComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryComponent {
    pub fn new() -> Self {
        Self { auto_scroll: true }
    }
}

impl Component for HistoryComponent {
    fn handle_event(&mut self, event: &Event, state: &mut AppState) -> Result<Option<TuiAction>> {
        if state.focus != crate::tui::state::Focus::History {
            return Ok(None);
        }

        if let Event::Key(key) = event {
            match key.code {
                KeyCode::Up => {
                    let max_scroll = state.history.len();
                    if state.scroll_offset < max_scroll {
                        state.scroll_offset += 1;
                        self.auto_scroll = false;
                    }
                }
                KeyCode::Down if state.scroll_offset > 0 => {
                    state.scroll_offset -= 1;
                    if state.scroll_offset == 0 {
                        self.auto_scroll = true;
                    }
                }
                KeyCode::PageUp => {
                    state.scroll_offset = state.scroll_offset.saturating_add(10);
                    self.auto_scroll = false;
                }
                KeyCode::PageDown => {
                    state.scroll_offset = state.scroll_offset.saturating_sub(10);
                    if state.scroll_offset == 0 {
                        self.auto_scroll = true;
                    }
                }
                KeyCode::Home => {
                    // Jump to top
                    state.scroll_offset = state.history.len();
                    self.auto_scroll = false;
                }
                KeyCode::End => {
                    // Jump to bottom — re-enable auto-scroll
                    state.scroll_offset = 0;
                    self.auto_scroll = true;
                }
                KeyCode::Char('g') => {
                    // vi-style: jump to bottom
                    state.scroll_offset = 0;
                    self.auto_scroll = true;
                }
                _ => {}
            }
        }
        Ok(None)
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Auto-scroll: ensure we're at the bottom when new content arrives
        if self.auto_scroll {
            // State is immutable here, but scroll_offset is managed externally.
            // The TUI tick handler already sets scroll_offset = 0 on new streaming content.
        }
        crate::tui::render::render_history_pane(frame, state, area);
    }
}
