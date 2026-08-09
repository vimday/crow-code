use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

use crate::tui::state::{AppState, Focus};
use crate::tui::theme::colors;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterMode {
    ShortcutOverlay,
    QuitConfirm,
    Running,
    Idle,
}

pub fn footer_mode(state: &AppState) -> FooterMode {
    if state.show_shortcuts_overlay {
        FooterMode::ShortcutOverlay
    } else if state
        .quit_hint_until
        .is_some_and(|t| std::time::Instant::now() < t)
    {
        FooterMode::QuitConfirm
    } else if state.is_task_running() {
        FooterMode::Running
    } else {
        FooterMode::Idle
    }
}

pub fn footer_height(state: &AppState) -> u16 {
    match footer_mode(state) {
        FooterMode::ShortcutOverlay => 7,
        FooterMode::QuitConfirm | FooterMode::Running | FooterMode::Idle => 1,
    }
}

pub fn footer_hint_lines(state: &AppState) -> Vec<Line<'static>> {
    match footer_mode(state) {
        FooterMode::ShortcutOverlay => vec![
            Line::from(vec![
                "  enter".bold().dim(),
                " send".dim(),
                "      ".into(),
                "esc".bold().dim(),
                " interrupt".dim(),
            ]),
            Line::from(vec![
                "  shift+enter".bold().dim(),
                " newline".dim(),
                "  ".into(),
                "ctrl+c".bold().dim(),
                " quit (×2)".dim(),
            ]),
            Line::from(vec![
                "  /".bold().dim(),
                " commands".dim(),
                "        ".into(),
                "tab".bold().dim(),
                " focus".dim(),
            ]),
            Line::from(vec![
                "  !".bold().dim(),
                " shell".dim(),
                "           ".into(),
                "ctrl+u".bold().dim(),
                " clear".dim(),
            ]),
            Line::from(vec![
                "  pgup/pgdn".bold().dim(),
                " scroll".dim(),
                "   ".into(),
                "↑/↓".bold().dim(),
                " history".dim(),
            ]),
            Line::from("  ? again to dismiss".dim()),
        ],
        FooterMode::QuitConfirm => vec![Line::from(vec![
            "  ".into(),
            "ctrl+c".bold().fg(colors::accent_warning()),
            " again to quit".fg(colors::accent_warning()),
        ])],
        FooterMode::Running => vec![Line::from(vec![
            "  ".into(),
            "esc".bold().dim(),
            " interrupt".dim(),
            " · ".dim(),
            "ctrl+c".bold().dim(),
            " cancel".dim(),
        ])],
        FooterMode::Idle => {
            let focus = match state.focus {
                Focus::Composer => "composer",
                Focus::History => "history",
                Focus::Explorer => "explorer",
            };
            vec![Line::from(vec![
                Span::from("  "),
                "?".bold().dim(),
                " keys".dim(),
                " · ".dim(),
                "/".bold().dim(),
                " commands".dim(),
                " · focus ".dim(),
                focus.dim(),
            ])]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: Line<'static>) -> String {
        line.spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect::<String>()
    }

    #[test]
    fn idle_footer_names_shortcuts_commands_and_focus() {
        let state = AppState::new("model".into(), "write".into(), "workspace".into());

        assert_eq!(footer_height(&state), 1);
        let text = line_text(footer_hint_lines(&state).remove(0));
        assert!(text.contains("? keys"));
        assert!(text.contains("/ commands"));
        assert!(text.contains("focus composer"));
    }

    #[test]
    fn running_footer_prefers_interrupt_and_cancel() {
        let mut state = AppState::new("model".into(), "write".into(), "workspace".into());
        state.active_action = Some("Working".into());

        let text = line_text(footer_hint_lines(&state).remove(0));
        assert!(text.contains("esc interrupt"));
        assert!(text.contains("ctrl+c cancel"));
    }
}
