use crate::tui::components::Component;
use crate::tui::state::TuiMessage;
use crossterm::event::Event;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

pub struct InfoBar {
    pub current_model: String,
    pub git_branch: String,
    pub is_dirty: bool,
}

impl Component for InfoBar {
    fn handle_event(&mut self, _event: &Event) -> Option<TuiMessage> {
        None
    }

    fn render(&self, f: &mut Frame, area: Rect) {
        let left = format!("{} | Crow Console 6.0", self.current_model);
        let right = if self.is_dirty {
            format!("{}*", self.git_branch)
        } else {
            self.git_branch.clone()
        };

        let content = Line::from(vec![
            Span::styled(left, Style::default().fg(Color::Cyan)),
            Span::raw(" | "),
            Span::styled(right, Style::default().fg(Color::Yellow)),
        ]);

        let p = Paragraph::new(content).block(Block::default().borders(Borders::NONE));
        f.render_widget(p, area);
    }
}
