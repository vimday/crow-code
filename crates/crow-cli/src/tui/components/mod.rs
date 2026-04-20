use crate::tui::state::TuiMessage;
use crossterm::event::Event;
use ratatui::layout::Rect;
use ratatui::Frame;

/// Core interface for Elm/Redux style UI components in Crow Console.
pub trait Component {
    /// Update the component state based on terminal events.
    /// Returns an optional TuiMessage to bubble up to the main application event bus.
    fn handle_event(&mut self, event: &Event) -> Option<TuiMessage>;

    /// Render the component onto the current frame buffer at the specified area.
    fn render(&self, f: &mut Frame, area: Rect);
}

pub mod chat_view;
pub mod command_palette;
pub mod info_bar;
pub mod composer;
pub mod history;
