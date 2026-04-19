use crate::tui::components::Component;
use crate::tui::state::{Cell, CellKind, TuiMessage};
use crossterm::event::Event;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

pub struct ChatView {
    pub cells: Vec<Cell>,
    pub scroll_offset: usize,
}

impl Component for ChatView {
    fn handle_event(&mut self, _event: &Event) -> Option<TuiMessage> {
        // Handle scrolling in the future
        None
    }

    fn render(&self, f: &mut Frame, area: Rect) {
        let mut list_items = Vec::new();

        for cell in &self.cells {
            match cell.kind {
                CellKind::User => {
                    list_items.push(ListItem::new(Line::from("")));
                    for (i, line) in cell.payload.lines().enumerate() {
                        let prefix = if i == 0 { "› " } else { "  " };
                        list_items.push(ListItem::new(Line::from(vec![
                            Span::styled(prefix, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                            Span::styled(line.to_string(), Style::default().fg(Color::White)),
                        ])));
                    }
                    list_items.push(ListItem::new(Line::from("")));
                }
                CellKind::AgentMessage => {
                    for (i, line) in cell.payload.lines().enumerate() {
                        let prefix = if i == 0 { "• " } else { "  " };
                        list_items.push(ListItem::new(Line::from(vec![
                            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                            Span::raw(line.to_string()),
                        ])));
                    }
                }
                CellKind::Action => {
                    list_items.push(ListItem::new(Line::from(vec![
                        Span::styled("  ▰ ", Style::default().fg(Color::Green)),
                        Span::styled(cell.payload.clone(), Style::default().fg(Color::Green)),
                    ])));
                }
                CellKind::Result => {
                    list_items.push(ListItem::new(Line::from(vec![
                        Span::styled("  ✓ ", Style::default().fg(Color::Blue)),
                        Span::styled(cell.payload.clone(), Style::default().fg(Color::Blue)),
                    ])));
                }
                CellKind::Log => {
                    for line in cell.payload.lines() {
                        list_items.push(ListItem::new(Line::from(vec![
                            Span::styled("  • ", Style::default().fg(Color::DarkGray)),
                            Span::styled(line.to_string(), Style::default().fg(Color::Gray)),
                        ])));
                    }
                }
                CellKind::Error => {
                    list_items.push(ListItem::new(Line::from(vec![
                        Span::styled("  ✘ ", Style::default().fg(Color::Red)),
                        Span::styled(cell.payload.clone(), Style::default().fg(Color::Red)),
                    ])));
                }
                CellKind::Evidence => {
                    list_items.push(ListItem::new(Line::from(vec![
                        Span::styled("  ◎ ", Style::default().fg(Color::DarkGray)),
                        Span::styled(cell.payload.clone(), Style::default().fg(Color::DarkGray)),
                    ])));
                }
            }
        }

        let list = List::new(list_items).block(Block::default().borders(Borders::NONE));
        f.render_widget(list, area);
    }
}
