use anyhow::Result;
use crossterm::event::Event;
use ratatui::{layout::Rect, Frame};
use super::state::AppState;

/// Signals dispatched by components communicating back to the main event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    /// Proceed to the next focus pane
    FocusNext,
    /// Yield visual control back to the normal app flow (for overlays)
    Dismiss,
    /// Component wants the app to submit text/command
    SubmitCommand(String),
}

/// The core architecture of modern Elm-like TUIs.
pub trait Component {
    /// Update internal state from events. Returns Some(Action) if it wishes to affect the global loop.
    fn handle_event(&mut self, event: &Event, state: &mut AppState) -> Result<Option<TuiAction>>;
    
    /// Pure rendering step for this particular modular region.
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState);
}
