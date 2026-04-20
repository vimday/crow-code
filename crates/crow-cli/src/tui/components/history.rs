use super::super::component::{Component, TuiAction};

use super::super::state::AppState;
use anyhow::Result;
use crossterm::event::{Event, KeyCode};
use ratatui::{
    layout::Rect,
    Frame,
};

pub struct HistoryComponent;

impl Default for HistoryComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryComponent {
    pub fn new() -> Self {
        Self
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
                    }
                }
                KeyCode::Down
                    if state.scroll_offset > 0 => {
                        state.scroll_offset -= 1;
                    }
                KeyCode::PageUp => {
                    state.scroll_offset = state.scroll_offset.saturating_add(10);
                }
                KeyCode::PageDown => {
                    state.scroll_offset = state.scroll_offset.saturating_sub(10);
                }
                _ => {}
            }
        }
        Ok(None)
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Delegate to the legacy render_history block inside render.rs for now.
        // We will move it here entirely later.
        crate::tui::render::render_history_pane(frame, state, area);
    }
}
