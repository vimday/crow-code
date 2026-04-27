//! Streaming status info bar — inspired by Yomi's `InfoBar` and
//! Codex's `StatusIndicatorWidget`.
//!
//! Displays: [LEFT: model + git] [CENTER: spinner + action + elapsed] [RIGHT: token gauge]
//! Uses the global theme system instead of raw Color values.

use crate::tui::component::Component;
use crate::tui::state::AppState;
use crate::tui::theme::{chars, colors, Styles};
use crossterm::event::Event;
use ratatui::layout::Rect;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Styled;
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

/// Compact elapsed time formatting (ported from Codex's `fmt_elapsed_compact`).
pub fn fmt_elapsed_compact(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        let minutes = secs / 60;
        let seconds = secs % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

/// Token usage color based on context window usage percentage.
fn usage_color(pct: f64) -> ratatui::style::Color {
    if pct < 0.5 {
        colors::accent_success()
    } else if pct < 0.9 {
        colors::accent_warning()
    } else {
        colors::accent_error()
    }
}

pub struct InfoBar;

impl Default for InfoBar {
    fn default() -> Self {
        Self::new()
    }
}

impl InfoBar {
    pub fn new() -> Self {
        Self
    }

    pub fn render_with_state(&self, f: &mut Frame, area: Rect, state: &AppState) {
        // Split into left info, center status, and right token bar
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Percentage(35),
                Constraint::Percentage(35),
            ])
            .split(area);

        // ── LEFT: model + branch ────────────────────────────────────
        let branch_display = if state.is_dirty {
            format!("{}*", state.git_branch)
        } else {
            state.git_branch.clone()
        };

        let model_display = if state.model_info.len() > 20 {
            format!("{}…", &state.model_info[..19])
        } else {
            state.model_info.clone()
        };

        let left = Line::from(vec![
            format!(" {model_display} ")
                .fg(colors::accent_system())
                .bold(),
            " │ ".fg(colors::divider()),
            format!(" {branch_display} ").fg(colors::accent_warning()),
        ]);

        let left_widget = Paragraph::new(left).block(Block::default().borders(Borders::NONE));
        f.render_widget(left_widget, chunks[0]);

        // ── CENTER: action + streaming metrics ──────────────────────
        let center_spans = if state.is_streaming {
            let spinner = chars::SPINNER[state.spinner_idx % chars::SPINNER.len()];
            let elapsed = state
                .streaming_start_time
                .map(|t| fmt_elapsed_compact(t.elapsed().as_secs()))
                .unwrap_or_default();

            let tokens = state.streaming_token_estimate;
            let token_display = if tokens < 1000.0 {
                format!("{tokens:.0} tok")
            } else {
                format!("{:.1}k tok", tokens / 1000.0)
            };

            vec![
                format!(" {spinner} ").set_style(Styles::spinner()),
                token_display.fg(colors::text_secondary()),
                format!(" · {elapsed}").fg(colors::text_secondary()),
            ]
        } else if let Some(ref action) = state.active_action {
            let spinner = chars::SPINNER[state.spinner_idx % chars::SPINNER.len()];
            let action_display = if action.len() > 30 {
                format!("{}…", &action[..29])
            } else {
                action.clone()
            };
            let elapsed = state
                .task_start_time
                .map(|t| format!(" {}s", t.elapsed().as_secs()))
                .unwrap_or_default();

            vec![
                format!(" {spinner} ").set_style(Styles::spinner()),
                action_display.fg(colors::text_muted()),
                elapsed.fg(colors::text_muted()),
            ]
        } else {
            vec![" Ready".fg(colors::text_muted())]
        };

        let center_widget =
            Paragraph::new(Line::from(center_spans)).block(Block::default().borders(Borders::NONE));
        f.render_widget(center_widget, chunks[1]);

        // ── RIGHT: token usage gauge (Yomi-style) ──────────────────
        #[allow(clippy::cast_precision_loss)]
        if let Some((total_tokens, context_window)) = state.ctx_usage {
            if context_window > 0 {
                let pct = total_tokens as f64 / context_window as f64;
                let label = format!(
                    " {}K / {}K ({:.0}%) ",
                    total_tokens / 1000,
                    context_window / 1000,
                    pct * 100.0,
                );

                let gauge_style = Style::new().fg(usage_color(pct)).bold();

                let gauge = Gauge::default()
                    .block(Block::default().borders(Borders::NONE))
                    .gauge_style(gauge_style)
                    .ratio(pct.min(1.0))
                    .label(label);
                f.render_widget(gauge, chunks[2]);
            } else {
                let placeholder = Paragraph::new(" Tokens: —".fg(colors::text_muted()));
                f.render_widget(placeholder, chunks[2]);
            }
        } else {
            let placeholder = Paragraph::new(" Tokens: —".fg(colors::text_muted()));
            f.render_widget(placeholder, chunks[2]);
        }
    }
}

impl Component for InfoBar {
    fn handle_event(
        &mut self,
        _event: &Event,
        _state: &mut AppState,
    ) -> anyhow::Result<Option<crate::tui::component::TuiAction>> {
        Ok(None)
    }

    fn render(&mut self, f: &mut Frame, area: Rect, state: &AppState) {
        // Delegate to render_with_state for backward compat
        self.render_with_state(f, area, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elapsed_compact_seconds() {
        assert_eq!(fmt_elapsed_compact(0), "0s");
        assert_eq!(fmt_elapsed_compact(59), "59s");
    }

    #[test]
    fn test_elapsed_compact_minutes() {
        assert_eq!(fmt_elapsed_compact(60), "1m 00s");
        assert_eq!(fmt_elapsed_compact(90), "1m 30s");
    }

    #[test]
    fn test_elapsed_compact_hours() {
        assert_eq!(fmt_elapsed_compact(3600), "1h 00m 00s");
        assert_eq!(fmt_elapsed_compact(3661), "1h 01m 01s");
    }
}
