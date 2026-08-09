use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::tui::state::{AgentHudEntry, AutoRunState};

const MAX_AGENT_LINES: usize = 6;
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

fn truncate_cells(text: &str, max_width: u16) -> String {
    let max_width = max_width as usize;
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width >= max_width {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push('…');
    out
}

fn line_clamped(text: impl Into<String>, max_width: u16) -> Line<'static> {
    Line::from(truncate_cells(&text.into(), max_width))
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
            "Progress: {}/{} complete · {} running · {} queued · {} failed",
            auto.completed_agents,
            auto.total_agents,
            auto.running_agents,
            auto.queued_agents,
            auto.failed_agents
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

pub fn render_cockpit_lines(auto: &AutoRunState, width: u16) -> Vec<Line<'static>> {
    let width = width.max(8);
    let Some(run_id) = auto.run_id.as_deref() else {
        return vec![line_clamped("run", width), line_clamped("  idle", width)];
    };

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::from("run").bold(),
        Span::from(format!(
            " · {}",
            truncate_cells(run_id, width.saturating_sub(7))
        ))
        .dim(),
    ]));

    if let Some(prompt) = auto.prompt.as_deref() {
        lines.push(line_clamped(format!("  {}", bounded_preview(prompt)), width).dim());
    }
    if let Some(phase) = auto.active_phase.as_deref() {
        lines.push(line_clamped(format!("  {phase}"), width));
    }
    if auto.total_agents > 0 {
        lines.push(line_clamped(
            format!(
                "  {}/{} · {} run · {} fail",
                auto.completed_agents, auto.total_agents, auto.running_agents, auto.failed_agents
            ),
            width,
        ));
    }

    if !auto.agents.is_empty() {
        lines.push(Line::from(""));
        lines.push(line_clamped("agents", width).bold());
        for agent in auto.agents.iter().take(MAX_AGENT_LINES) {
            let glyph = match agent.success {
                Some(true) => "✓",
                Some(false) => "✗",
                None if agent.status == "Running" => "●",
                None => "•",
            };
            lines.push(line_clamped(
                format!("  {glyph} {} [{}] {}", agent.name, agent.role, agent.status),
                width,
            ));
            if let Some(preview) = agent.preview.as_deref() {
                lines
                    .push(line_clamped(format!("    └ {}", bounded_preview(preview)), width).dim());
            }
        }
    }

    if !auto.recent_artifacts.is_empty() {
        lines.push(Line::from(""));
        lines.push(line_clamped("drops", width).bold());
        for artifact in auto.recent_artifacts.iter().rev().take(3) {
            let compact = bounded_preview(artifact);
            let compact = compact
                .split_once(':')
                .map(|(_, tail)| tail.trim())
                .unwrap_or(compact.as_str());
            lines.push(line_clamped(format!("  artifact {compact}"), width));
        }
    }

    if let Some(summary) = auto.last_summary.as_deref() {
        lines.push(Line::from(""));
        lines.push(line_clamped("summary", width).bold());
        lines.push(line_clamped(format!("  {}", bounded_preview(summary)), width).dim());
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
    fn cockpit_lines_include_run_progress_agents_and_artifacts() {
        let auto = AutoRunState {
            run_id: Some("auto-42".into()),
            prompt: Some("redesign the tui into a cockpit".into()),
            active_phase: Some("Synthesis".into()),
            total_agents: 4,
            completed_agents: 2,
            running_agents: 1,
            queued_agents: 1,
            failed_agents: 1,
            cancelled_agents: 0,
            agents: vec![AgentHudEntry {
                id: "agent-a".into(),
                name: "planner".into(),
                role: "architect".into(),
                phase: "Synthesis".into(),
                status: "Running".into(),
                preview: Some("mapping Codex chatwidget and multi-agent UX into Crow".into()),
                done: false,
                success: None,
            }],
            recent_artifacts: vec!["Design: cockpit frame ready".into()],
            last_summary: Some("auto run completed with one verifier failure".into()),
        };

        let lines = render_cockpit_lines(&auto, 36);
        let rendered = lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("run"));
        assert!(rendered.contains("auto-42"));
        assert!(rendered.contains("2/4"));
        assert!(rendered.contains("planner"));
        assert!(rendered.contains("artifact"));
        assert!(rendered.contains("cockpit frame ready"));
    }

    #[test]
    fn cockpit_lines_stay_within_requested_width() {
        let auto = AutoRunState {
            run_id: Some("auto-wide".into()),
            prompt: Some("x".repeat(200)),
            active_phase: Some("VeryLongPhaseNameThatShouldBeTrimmed".into()),
            total_agents: 1,
            completed_agents: 0,
            running_agents: 1,
            queued_agents: 0,
            failed_agents: 0,
            cancelled_agents: 0,
            agents: vec![AgentHudEntry {
                id: "agent-long".into(),
                name: "extremely-long-agent-name".into(),
                role: "very-long-role".into(),
                phase: "VeryLongPhaseNameThatShouldBeTrimmed".into(),
                status: "Running".into(),
                preview: Some("preview ".repeat(80)),
                done: false,
                success: None,
            }],
            recent_artifacts: vec!["artifact ".repeat(80)],
            last_summary: None,
        };

        for line in render_cockpit_lines(&auto, 24) {
            assert!(line.width() <= 24, "line exceeded width: {line:?}");
        }
    }
}
