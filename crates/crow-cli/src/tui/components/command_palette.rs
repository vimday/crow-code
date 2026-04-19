use crate::tui::components::Component;
use crate::tui::state::TuiMessage;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

pub struct CommandPalette {
    pub active: bool,
    pub composer: String,
    pub cursor: usize,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            active: true,
            composer: String::new(),
            cursor: 0,
        }
    }
}

impl Component for CommandPalette {
    fn handle_event(&mut self, event: &Event) -> Option<TuiMessage> {
        if !self.active {
            return None;
        }

        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Char(c) => {
                        self.composer.insert(self.cursor, c);
                        self.cursor += 1;
                        return Some(TuiMessage::Tick);
                    }
                    KeyCode::Backspace if self.cursor > 0 => {
                        self.cursor -= 1;
                        self.composer.remove(self.cursor);
                        return Some(TuiMessage::Tick);
                    }
                    KeyCode::Left => {
                        self.cursor = self.cursor.saturating_sub(1);
                        return Some(TuiMessage::Tick);
                    }
                    KeyCode::Right if self.cursor < self.composer.len() => {
                        self.cursor += 1;
                        return Some(TuiMessage::Tick);
                    }
                    KeyCode::Enter => {
                        // Submit logic happens in the main event router currently,
                        // but we could emit a custom command execution message.
                    }
                    _ => {}
                }
            }
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect) {
        let palette_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " Command Palette ",
                Style::default().add_modifier(Modifier::BOLD),
            ));

        let input_text = format!("> {}", self.composer);

        let p = Paragraph::new(input_text).block(palette_block);
        f.render_widget(p, area);

        f.set_cursor(area.x + 3 + self.cursor as u16, area.y + 1);
    }
}
