use crate::tui::components::Component;
use crate::tui::state::TuiMessage;
use crossterm::event::Event;
use ratatui::layout::Rect;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

/// Token usage state tracked from AgentEvent::TokenUsage
#[derive(Default, Clone)]
pub struct TokenState {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub context_window: u32,
}

impl TokenState {
    pub fn usage_pct(&self) -> f64 {
        if self.context_window == 0 {
            return 0.0;
        }
        self.total_tokens as f64 / self.context_window as f64
    }

    /// Color based on usage percentage
    pub fn bar_color(&self) -> Color {
        let pct = self.usage_pct();
        if pct < 0.5 {
            Color::Green
        } else if pct < 0.75 {
            Color::Yellow
        } else if pct < 0.9 {
            Color::Rgb(255, 140, 0) // orange
        } else {
            Color::Red
        }
    }
}

pub struct InfoBar {
    pub current_model: String,
    pub git_branch: String,
    pub is_dirty: bool,
    pub token_state: TokenState,
}

impl Component for InfoBar {
    fn handle_event(&mut self, _event: &Event) -> Option<TuiMessage> {
        None
    }

    fn render(&self, f: &mut Frame, area: Rect) {
        // Split into left info and right token bar
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);

        // Left: model + branch info
        let branch_display = if self.is_dirty {
            format!("{}*", self.git_branch)
        } else {
            self.git_branch.clone()
        };

        let left = Line::from(vec![
            format!(" {} ", self.current_model).cyan().bold(),
            " │ ".dark_gray(),
            format!(" {branch_display} ").yellow(),
        ]);

        let left_widget = Paragraph::new(left).block(Block::default().borders(Borders::NONE));
        f.render_widget(left_widget, chunks[0]);

        // Right: token usage gauge (Yomi-style usage bar)
        if self.token_state.context_window > 0 {
            let pct = self.token_state.usage_pct();
            let label = format!(
                " {}K / {}K ({:.0}%) ",
                self.token_state.total_tokens / 1000,
                self.token_state.context_window / 1000,
                pct * 100.0,
            );

            // Cannot easily eliminate Style entirely here since color is dynamic,
            // but we can start with Style::default() and combine Stylize.
            let gauge_style = ratatui::style::Style::default()
                .fg(self.token_state.bar_color())
                .bold();

            let gauge = Gauge::default()
                .block(Block::default().borders(Borders::NONE))
                .gauge_style(gauge_style)
                .ratio(pct.min(1.0))
                .label(label);
            f.render_widget(gauge, chunks[1]);
        } else {
            // No token data yet — show placeholder
            let placeholder = Paragraph::new(" Tokens: —".dark_gray());
            f.render_widget(placeholder, chunks[1]);
        }
    }
}
