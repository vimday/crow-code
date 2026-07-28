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
    if auto.total_agents > 0 {
        lines.push(format!(
            "Progress: {}/{} complete · {} running · {} failed",
            auto.completed_agents, auto.total_agents, auto.running_agents, auto.failed_agents
        ));
    }
    if let Some(latest) = auto.recent_artifacts.last() {
        lines.push(format!("Latest artifact: {}", bounded_preview(latest)));
    }
    if let Some(summary) = auto.last_summary.as_deref() {
        lines.push(format!("Summary: {}", bounded_preview(summary)));
    }
    for agent in auto.agents.iter().take(MAX_AGENT_LINES) {
        lines.push(format!(
            "- {} [{}] {}",
            agent.name, agent.role, agent.status
        ));
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
        status,
        preview,
        success,
        ..
    } in auto.agents.iter().take(MAX_AGENT_LINES)
    {
        let glyph = match success {
            Some(true) => "✓",
            Some(false) => "✗",
            None if status == "Running" => "●",
            None => "•",
        };
        lines.push(Line::from(format!("  {glyph} {name} [{role}] {status}")));
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

    #[test]
    fn summary_includes_progress_and_last_summary() {
        let auto = AutoRunState {
            run_id: Some("auto-1".into()),
            prompt: Some("ship it".into()),
            active_phase: Some("Review".into()),
            total_agents: 3,
            completed_agents: 2,
            running_agents: 1,
            failed_agents: 0,
            cancelled_agents: 0,
            agents: Vec::new(),
            recent_artifacts: vec!["Review: no blockers".into()],
            last_summary: Some("reviewing final evidence".into()),
        };

        let summary = format_agent_summary(&auto);
        assert!(summary.contains("Phase: Review"));
        assert!(summary.contains("Progress: 2/3 complete · 1 running · 0 failed"));
        assert!(summary.contains("Latest artifact: Review: no blockers"));
        assert!(summary.contains("Summary: reviewing final evidence"));
    }
}
