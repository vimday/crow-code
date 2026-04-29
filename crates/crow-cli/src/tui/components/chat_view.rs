//! Legacy chat view component — superseded by the trait-based `HistoryCell` system
//! in `history_cell.rs` + `render.rs`. Kept for reference but all methods are dead code.

use crate::tui::history_cell::HistoryCell;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

pub struct ChatView {
    pub cells: Vec<Box<dyn HistoryCell>>,
    pub scroll_offset: usize,
}

impl ChatView {
    #[allow(dead_code)]
    pub fn handle_event(&mut self) {
        // Handle scrolling in the future
    }

    #[allow(dead_code)]
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let mut list_items = Vec::new();

        for cell in &self.cells {
            let lines = cell.display_lines(area.width);
            for line in lines {
                list_items.push(ListItem::new(line));
            }
        }

        let list = List::new(list_items).block(Block::default().borders(Borders::NONE));
        f.render_widget(list, area);
    }
}
