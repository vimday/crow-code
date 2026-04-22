use crate::tui::state::{Cell, CellKind};
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

pub struct ChatView {
    pub cells: Vec<Cell>,
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
            match cell.kind {
                CellKind::User => {
                    list_items.push(ListItem::new(Line::from("")));
                    for (i, line) in cell.payload.lines().enumerate() {
                        let prefix = if i == 0 { "› " } else { "  " };
                        list_items.push(ListItem::new(Line::from(vec![
                            prefix.cyan().bold(),
                            line.white(),
                        ])));
                    }
                    list_items.push(ListItem::new(Line::from("")));
                }
                CellKind::AgentMessage => {
                    for (i, line) in cell.payload.lines().enumerate() {
                        let prefix = if i == 0 { "• " } else { "  " };
                        list_items.push(ListItem::new(Line::from(vec![
                            prefix.dark_gray(),
                            line.into(),
                        ])));
                    }
                }
                CellKind::Action => {
                    list_items.push(ListItem::new(Line::from(vec![
                        "  ▰ ".green(),
                        cell.payload.clone().green(),
                    ])));
                }
                CellKind::Result => {
                    list_items.push(ListItem::new(Line::from(vec![
                        "  ✓ ".blue(),
                        cell.payload.clone().blue(),
                    ])));
                }
                CellKind::Log => {
                    for line in cell.payload.lines() {
                        list_items.push(ListItem::new(Line::from(vec![
                            "  • ".dark_gray(),
                            line.gray(),
                        ])));
                    }
                }
                CellKind::Error => {
                    list_items.push(ListItem::new(Line::from(vec![
                        "  ✘ ".red(),
                        cell.payload.clone().red(),
                    ])));
                }
                CellKind::Evidence => {
                    list_items.push(ListItem::new(Line::from(vec![
                        "  ◎ ".dark_gray(),
                        cell.payload.clone().dark_gray(),
                    ])));
                }
                CellKind::Debate => {
                    list_items.push(ListItem::new(Line::from(vec![
                        "  ⚖ ".magenta(),
                        cell.payload.clone().magenta(),
                    ])));
                }
            }
        }

        let list = List::new(list_items).block(Block::default().borders(Borders::NONE));
        f.render_widget(list, area);
    }
}
