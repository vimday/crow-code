use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::Stylize;
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

#[allow(dead_code)]
impl CommandPalette {
    pub fn handle_event_legacy(&mut self, event: &Event) {
        if !self.active {
            return;
        }

        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Char(c) => {
                        self.composer.insert(self.cursor, c);
                        self.cursor += 1;
                    }
                    KeyCode::Backspace if self.cursor > 0 => {
                        self.cursor -= 1;
                        self.composer.remove(self.cursor);
                    }
                    KeyCode::Left => {
                        self.cursor = self.cursor.saturating_sub(1);
                    }
                    KeyCode::Right if self.cursor < self.composer.len() => {
                        self.cursor += 1;
                    }
                    KeyCode::Enter => {
                        // Submit logic happens in the main event router currently.
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let palette_block = Block::default()
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::new().dark_gray())
            .title(" Command Palette ".bold());

        let input_text = format!("> {}", self.composer);

        let p = Paragraph::new(input_text).block(palette_block);
        f.render_widget(p, area);

        f.set_cursor(area.x + 3 + self.cursor as u16, area.y + 1);
    }
}
