use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMode {
    Narrow,
    Standard,
    Cockpit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLayout {
    pub mode: FrameMode,
    pub main: Rect,
    pub cockpit: Option<Rect>,
}

pub fn compute_frame_layout(area: Rect) -> FrameLayout {
    if area.width < 100 {
        return FrameLayout {
            mode: FrameMode::Narrow,
            main: area,
            cockpit: None,
        };
    }

    let side_width = if area.width >= 160 { 36 } else { 24 };
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(area.width.saturating_sub(side_width)),
            Constraint::Length(side_width),
        ])
        .split(area);

    FrameLayout {
        mode: if area.width >= 160 {
            FrameMode::Cockpit
        } else {
            FrameMode::Standard
        },
        main: chunks[0],
        cockpit: Some(chunks[1]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn narrow_terminals_hide_cockpit() {
        let frame = compute_frame_layout(Rect::new(0, 0, 76, 30));

        assert_eq!(frame.mode, FrameMode::Narrow);
        assert!(frame.cockpit.is_none());
        assert_eq!(frame.main.width, 76);
    }

    #[test]
    fn standard_terminals_use_compact_cockpit() {
        let frame = compute_frame_layout(Rect::new(0, 0, 120, 32));

        assert_eq!(frame.mode, FrameMode::Standard);
        assert_eq!(frame.cockpit.expect("cockpit visible").width, 24);
        assert_eq!(frame.main.width, 96);
    }

    #[test]
    fn wide_terminals_use_full_cockpit() {
        let frame = compute_frame_layout(Rect::new(0, 0, 180, 40));

        assert_eq!(frame.mode, FrameMode::Cockpit);
        assert_eq!(frame.cockpit.expect("cockpit visible").width, 36);
        assert_eq!(frame.main.width, 144);
    }
}
