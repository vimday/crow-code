use ratatui::{backend::CrosstermBackend, layout::{Constraint, Direction, Layout}, style::Color, widgets::{Block, Borders, Clear, List, ListItem}, Terminal};
use std::io;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|f| {
        let area = f.area();
        let popup_h = 0;
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(popup_h), Constraint::Min(0)])
            .split(area);
            
        let popup_area = split[0];
        let composer_area = split[1];
        
        let composer_split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(composer_area);
            
        let prompt_widget = ratatui::widgets::Paragraph::new("❯ ");
        f.render_widget(prompt_widget, composer_split[0]);
    })?;

    Ok(())
}
