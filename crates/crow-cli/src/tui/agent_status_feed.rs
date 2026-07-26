use ratatui::style::Stylize;
use ratatui::text::Line;

use crate::tui::state::{AgentHudEntry, AutoRunState};

const MAX_AGENT_LINES: usize = 8;
const MAX_PREVIEW_CHARS: usize = 240;

pub fn bounded_preview(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_PREVIEW_CHARS {
        return compact;
    }
    let mut out = compact
        .chars()
        .take(MAX_PREVIEW_CHARS.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

pub fn format_agent_summary(auto: &AutoRunState) -> String {
    let Some(run_id) = auto.run_id.as_deref() else {
        return "No active auto-mode run.".to_string();
    };
    let mut lines = vec![format!("Auto run {run_id}")];
    if let Some(phase) = auto.active_phase.as_deref() {
        lines.push(format!("Phase: {phase}"));
    }
    for agent in auto.agents.iter().take(MAX_AGENT_LINES) {
        let status = match agent.success {
            Some(true) => "done",
            Some(false) => "failed",
            None if agent.done => "done",
            None => "running",
        };
        lines.push(format!("- {} [{}] {status}", agent.name, agent.role));
        if let Some(preview) = agent.preview.as_deref() {
            lines.push(format!("  └ {}", bounded_preview(preview)));
        }
    }
    lines.join("\n")
}

pub fn render_agent_lines(auto: &AutoRunState) -> Vec<Line<'static>> {
    if auto.run_id.is_none() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let title = match auto.active_phase.as_deref() {
        Some(phase) => format!("agents · {phase}"),
        None => "agents".to_string(),
    };
    lines.push(Line::from(title).bold());
    for AgentHudEntry {
        name,
        role,
        preview,
        success,
        ..
    } in auto.agents.iter().take(MAX_AGENT_LINES)
    {
        let glyph = match success {
            Some(true) => "✓",
            Some(false) => "✗",
            None => "•",
        };
        lines.push(Line::from(format!("  {glyph} {name} [{role}]")));
        if let Some(preview) = preview {
            lines.push(Line::from(format!("    └ {}", bounded_preview(preview))).dim());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_preview_compacts_and_truncates() {
        let text = "x ".repeat(400);
        let preview = bounded_preview(&text);
        assert!(preview.chars().count() <= 240);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn empty_summary_is_clear() {
        assert_eq!(
            format_agent_summary(&AutoRunState::default()),
            "No active auto-mode run."
        );
    }
}
