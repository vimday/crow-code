use ratatui::widgets::{Block, Borders};
use ratatui::{backend::CrosstermBackend,Terminal};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use tui_textarea::TextArea;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, KeyEventKind};

fn main() {
    let mut textarea = TextArea::default();
    
    // Simulate typing /view<space>
    textarea.input(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    textarea.input(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    textarea.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    textarea.input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    textarea.input(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
    textarea.input(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    
    let lines = textarea.lines();
    if lines.len() == 1 && (lines[0].starts_with('/') || lines[0].starts_with('!')) {
        let query = lines[0].to_string();
        println!("query: '{}'", query);
    }
}
