use crate::event::{AgentEvent, ViewMode};
use crow_patch::SnapshotId;
pub use crow_runtime::cancel::CancellationToken;
use std::time::Instant;

use super::history_cell;

// ── TUI Message Bus ──────────────────────────────────────────────────────────

pub enum TuiMessage {
    AgentEvent(AgentEvent),
    TurnComplete(bool, Option<crow_runtime::event::TurnTimingSummary>),
    SessionComplete,
    SwarmStarted(String, String),
    SwarmComplete(String, bool),
    Tick,
    /// Clean exit requested (e.g. via `/exit` or `/quit` command).
    Quit,
}

// ── Re-export the HistoryCell trait for convenience ──────────────────────────

pub use super::history_cell::HistoryCell;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusIndicatorState {
    pub header: String,
    pub details: Option<String>,
    pub details_max_lines: usize,
    /// Optional progress (0.0–1.0) — when set, the status indicator
    /// renders a slim progress bar below the header. Stored as integer
    /// percentage (0..=100) so the struct stays Eq-comparable.
    pub progress_pct: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentHudEntry {
    pub id: String,
    pub name: String,
    pub role: String,
    pub phase: String,
    pub status: String,
    pub preview: Option<String>,
    pub done: bool,
    pub success: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoRunState {
    pub run_id: Option<String>,
    pub prompt: Option<String>,
    pub active_phase: Option<String>,
    pub total_agents: usize,
    pub completed_agents: usize,
    pub running_agents: usize,
    pub failed_agents: usize,
    pub cancelled_agents: usize,
    pub agents: Vec<AgentHudEntry>,
    pub recent_artifacts: Vec<String>,
    pub last_summary: Option<String>,
}

impl AutoRunState {
    fn reset_for_run(&mut self, run_id: &str) {
        if self.run_id.as_deref() != Some(run_id) {
            self.run_id = Some(run_id.to_string());
            self.agents.clear();
            self.recent_artifacts.clear();
            self.completed_agents = 0;
            self.running_agents = 0;
            self.failed_agents = 0;
            self.cancelled_agents = 0;
        }
    }

    fn upsert_agent(&mut self, run_id: &str, agent_id: &str, name: String, role: String) {
        self.reset_for_run(run_id);
        if let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == agent_id) {
            agent.name = name;
            agent.role = role;
            return;
        }
        self.agents.push(AgentHudEntry {
            id: agent_id.to_string(),
            name,
            role,
            phase: self.active_phase.clone().unwrap_or_default(),
            status: String::from("Queued"),
            preview: None,
            done: false,
            success: None,
        });
        self.agents.truncate(8);
    }

    fn mark_phase(&mut self, run_id: &str, phase: String) {
        self.reset_for_run(run_id);
        self.active_phase = Some(phase.clone());
        for agent in &mut self.agents {
            if !agent.done {
                agent.phase = phase.clone();
            }
        }
    }

    fn mark_agent_running(&mut self, run_id: &str, agent_id: &str, phase: String) {
        self.reset_for_run(run_id);
        self.upsert_agent(
            run_id,
            agent_id,
            agent_id.to_string(),
            String::from("agent"),
        );
        if let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == agent_id) {
            agent.phase = phase;
            agent.status = String::from("Running");
            agent.done = false;
            agent.success = None;
        }
        self.recount_agents();
    }

    fn mark_agent_completed(&mut self, run_id: &str, agent_id: &str, success: bool) {
        self.reset_for_run(run_id);
        if let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == agent_id) {
            agent.done = true;
            agent.success = Some(success);
            agent.status = if success {
                String::from("Completed")
            } else {
                String::from("Failed")
            };
        }
        self.recount_agents();
    }

    fn push_artifact_preview(&mut self, title: &str, preview: &str) {
        self.recent_artifacts
            .push(format!("{}: {}", title, truncate_preview(preview)));
        if self.recent_artifacts.len() > 4 {
            let overflow = self.recent_artifacts.len() - 4;
            self.recent_artifacts.drain(0..overflow);
        }
    }

    fn finish_run(&mut self, run_id: &str, summary: String, success: bool) {
        self.reset_for_run(run_id);
        self.last_summary = Some(summary);
        self.active_phase = if success {
            None
        } else {
            Some(String::from("Stopped"))
        };
        self.recount_agents();
    }

    fn recount_agents(&mut self) {
        self.completed_agents = self.agents.iter().filter(|agent| agent.done).count();
        self.running_agents = self
            .agents
            .iter()
            .filter(|agent| agent.status == "Running")
            .count();
        self.failed_agents = self
            .agents
            .iter()
            .filter(|agent| agent.success == Some(false))
            .count();
        self.cancelled_agents = self
            .agents
            .iter()
            .filter(|agent| agent.status == "Cancelled")
            .count();
    }
}

fn truncate_preview(preview: &str) -> String {
    let compact = preview.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 240 {
        return compact;
    }
    let mut out = compact.chars().take(239).collect::<String>();
    out.push('…');
    out
}

impl StatusIndicatorState {
    pub fn working() -> Self {
        Self {
            header: String::from("Working"),
            details: None,
            details_max_lines: 3,
            progress_pct: None,
        }
    }
}

// ── App State ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum ApprovalState {
    None,
    PendingCommand(String, usize),
}

#[derive(Clone, PartialEq, Eq)]
pub enum Focus {
    Composer,
    Explorer,
    History,
}

pub struct AppState {
    // Composer Advanced State
    pub composer: String,
    pub composer_cursor: usize,
    pub input_history: Vec<String>,
    pub input_history_idx: Option<usize>,

    // View
    pub view_mode: ViewMode,
    pub history: Vec<Box<dyn HistoryCell>>,
    pub active_cell: Option<Box<dyn HistoryCell>>,
    /// Raw streaming text buffer for the active agent message.
    /// We accumulate deltas here and rebuild `active_cell` from it,
    /// avoiding the need to downcast the trait object.
    pub streaming_buffer: String,
    pub scroll_offset: usize,

    // Runtime
    pub current_turn_id: Option<SnapshotId>,
    pub active_action: Option<String>,
    pub task_start_time: Option<Instant>,
    pub spinner_idx: usize,
    pub cancellation: Option<CancellationToken>,
    pub active_swarms: Vec<(String, String)>,
    pub task_queue: std::collections::VecDeque<String>,
    pub auto_run: AutoRunState,

    // Approval Model
    pub approval_state: ApprovalState,
    pub allowed_safe_patterns: std::collections::HashSet<String>,

    // Quit state (Codex-style: Ctrl+C twice to quit)
    pub last_ctrl_c: Option<Instant>,

    // Status Substrate Context
    pub model_info: String,
    pub write_mode: String,
    pub workspace_name: String,
    pub git_branch: String,
    pub is_dirty: bool,
    pub focus: Focus,

    // Incremental Markdown Streaming (Yomi-inspired)
    pub stream_state: crate::render::MarkdownStreamState,

    // Smooth streaming animation controller (CommitTick pattern)
    pub stream_controller: crate::tui::stream_controller::StreamController,

    // ── Streaming Metrics (Yomi InfoBar pattern) ─────────────────────
    /// Whether the agent is actively streaming LLM output.
    pub is_streaming: bool,
    /// Approximate token count accumulated during the current streaming turn.
    pub streaming_token_estimate: f64,
    /// When the current streaming turn started (for elapsed time display).
    pub streaming_start_time: Option<Instant>,
    /// When the active streaming cell was last rebuilt (throttle at 50ms).
    pub last_cell_update: Option<Instant>,
    /// Buffer length at last cell rebuild (throttle at 100-char delta).
    pub last_cell_buffer_len: usize,
    /// The agent's current deterministic execution phase (if active)
    pub turn_phase: Option<String>,

    // ── Context Window Usage (Yomi StatusBar pattern) ────────────────
    /// Last known total token usage and context window size.
    pub ctx_usage: Option<(u32, u32)>,

    // ── Cumulative Token Usage (session-level) ──────────────────────
    /// Total prompt tokens accumulated across all turns in this session.
    pub cumulative_prompt_tokens: u32,
    /// Total completion tokens accumulated across all turns in this session.
    pub cumulative_completion_tokens: u32,

    // ── Timed Status Messages (Yomi StatusMessage pattern) ──────────
    /// Transient message displayed in the status bar center section.
    pub status_message: Option<StatusMessage>,
    /// When the status message should auto-clear.
    pub status_message_timeout: Option<Instant>,

    // ── Deep Status Indicator (Codex pattern) ──────────────────────
    pub status_indicator: Option<StatusIndicatorState>,

    // ── Shortcut Overlay (Codex `?` key pattern) ────────────────────
    /// When true, the shortcut help overlay is visible.
    pub show_shortcuts_overlay: bool,

    // ── Quit Hint (Codex "press again to quit" pattern) ─────────────
    /// When set, display "Ctrl+C again to quit" until this instant.
    pub quit_hint_until: Option<Instant>,

    // ── Session Start (for /status duration tracking) ───────────────
    /// When this TUI session was created.
    pub session_start: Instant,
}

/// Transient status bar message with severity level and optional auto-clear.
#[derive(Clone, Debug)]
pub struct StatusMessage {
    pub content: String,
    pub level: StatusLevel,
}

/// Severity level for timed status messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Warn,
    Error,
    Tip,
}

impl StatusMessage {
    pub fn info(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            level: StatusLevel::Info,
        }
    }
    pub fn warn(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            level: StatusLevel::Warn,
        }
    }
    pub fn tip(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            level: StatusLevel::Tip,
        }
    }
}

// ── Convenience constructors for pushing cells ──────────────────────────────

impl AppState {
    pub fn push_user(&mut self, payload: impl Into<String>) {
        self.history.push(Box::new(history_cell::UserMessageCell {
            payload: payload.into(),
        }));
    }

    pub fn push_agent(&mut self, payload: impl Into<String>) {
        self.history.push(Box::new(history_cell::AgentMessageCell {
            payload: payload.into(),
            is_continuation: false,
        }));
    }

    pub fn push_evidence(&mut self, payload: impl Into<String>) {
        self.history.push(Box::new(history_cell::EvidenceCell {
            payload: payload.into(),
        }));
    }

    pub fn push_action(&mut self, payload: impl Into<String>) {
        self.history.push(Box::new(history_cell::ActionCell {
            payload: payload.into(),
        }));
    }

    pub fn push_result(&mut self, payload: impl Into<String>) {
        self.history.push(Box::new(history_cell::ResultCell {
            payload: payload.into(),
        }));
    }

    pub fn push_log(&mut self, payload: impl Into<String>) {
        self.history.push(Box::new(history_cell::LogCell {
            payload: payload.into(),
        }));
    }

    pub fn push_error(&mut self, payload: impl Into<String>) {
        self.history.push(Box::new(history_cell::ErrorCell {
            payload: payload.into(),
        }));
    }

    pub fn push_debate(&mut self, payload: impl Into<String>) {
        self.history.push(Box::new(history_cell::DebateCell {
            payload: payload.into(),
        }));
    }

    pub fn push_diff(&mut self, payload: impl Into<String>) {
        self.history.push(Box::new(history_cell::DiffCell {
            payload: payload.into(),
        }));
    }

    /// Push a polished tool-call card (replaces the flat ActionCell on
    /// tool completion).
    #[allow(clippy::too_many_arguments)]
    pub fn push_tool_card(
        &mut self,
        tool_name: String,
        duration_ms: u64,
        output_bytes: usize,
        is_error: bool,
        preview: String,
        retry_count: u32,
        from_cache: bool,
    ) {
        self.history
            .push(Box::new(history_cell::ToolCallCell::from_completed(
                tool_name,
                duration_ms,
                output_bytes,
                is_error,
                preview,
                retry_count,
                from_cache,
            )));
    }

    /// Push a turn completion summary separator.
    pub fn push_summary(&mut self, tool_count: usize, duration_ms: u64, tokens: u64) {
        self.history.push(Box::new(history_cell::SummaryCell {
            tool_count,
            duration_ms,
            tokens,
        }));
    }
}

impl AppState {
    pub fn new(model_info: String, write_mode: String, workspace_name: String) -> Self {
        Self {
            composer: String::new(),
            composer_cursor: 0,
            input_history: Vec::new(),
            input_history_idx: None,
            view_mode: ViewMode::default(),
            history: Vec::new(),
            active_cell: None,
            streaming_buffer: String::new(),
            scroll_offset: 0,
            current_turn_id: None,
            active_action: None,
            task_start_time: None,
            spinner_idx: 0,
            cancellation: None,
            active_swarms: Vec::new(),
            task_queue: std::collections::VecDeque::new(),
            auto_run: AutoRunState::default(),
            approval_state: ApprovalState::None,
            allowed_safe_patterns: std::collections::HashSet::new(),

            last_ctrl_c: None,
            model_info,
            write_mode,
            workspace_name,
            git_branch: "detecting...".into(),
            is_dirty: false,
            focus: Focus::Composer,
            stream_state: crate::render::MarkdownStreamState::default(),
            stream_controller: crate::tui::stream_controller::StreamController::new(),

            is_streaming: false,
            streaming_token_estimate: 0.0,
            streaming_start_time: None,
            last_cell_update: None,
            last_cell_buffer_len: 0,
            turn_phase: None,
            ctx_usage: None,
            cumulative_prompt_tokens: 0,
            cumulative_completion_tokens: 0,
            status_message: None,
            status_message_timeout: None,
            status_indicator: None,
            show_shortcuts_overlay: false,
            quit_hint_until: None,
            session_start: Instant::now(),
        }
    }

    pub fn is_task_running(&self) -> bool {
        self.active_action.is_some()
    }

    /// Show a status message with an auto-clear timeout (in milliseconds).
    /// Pass `0` for no timeout (persists until explicitly cleared).
    pub fn show_status(&mut self, msg: StatusMessage, timeout_ms: u64) {
        if timeout_ms == 0 {
            self.status_message_timeout = None;
        } else {
            self.status_message_timeout =
                Some(Instant::now() + std::time::Duration::from_millis(timeout_ms));
        }
        self.status_message = Some(msg);
    }

    /// Tick-driven: auto-clear expired status messages.
    pub fn check_status_timeout(&mut self) {
        if let Some(deadline) = self.status_message_timeout {
            if Instant::now() > deadline {
                self.status_message = None;
                self.status_message_timeout = None;
            }
        }
    }

    /// Approximate token estimation (Yomi pattern: ~4 chars per token).
    pub fn estimate_tokens(text: &str) -> f64 {
        text.len() as f64 / 4.0
    }

    pub fn apply_orchestration_event(&mut self, ev: &crow_runtime::event::OrchestrationEvent) {
        use crow_runtime::event::OrchestrationEvent;

        match ev {
            OrchestrationEvent::AutoStarted {
                run_id,
                prompt,
                agent_count,
            } => {
                self.auto_run = AutoRunState {
                    run_id: Some(run_id.clone()),
                    prompt: Some(prompt.clone()),
                    active_phase: Some(String::from("Preparing")),
                    total_agents: *agent_count,
                    completed_agents: 0,
                    running_agents: 0,
                    failed_agents: 0,
                    cancelled_agents: 0,
                    agents: Vec::with_capacity(*agent_count),
                    recent_artifacts: Vec::new(),
                    last_summary: None,
                };
                self.active_action = Some(format!("Auto mode running {agent_count} agents…"));
            }
            OrchestrationEvent::PhaseStarted { run_id, phase } => {
                self.auto_run.run_id = Some(run_id.clone());
                self.auto_run.mark_phase(run_id, phase.clone());
                self.active_action = Some(format!("Auto phase: {phase}"));
            }
            OrchestrationEvent::AgentStarted {
                run_id,
                agent_id,
                name,
                role,
            } => {
                self.auto_run
                    .upsert_agent(run_id, agent_id, name.clone(), role.clone());
            }
            OrchestrationEvent::AgentPreview {
                run_id,
                agent_id,
                preview,
            } => {
                self.auto_run.run_id = Some(run_id.clone());
                if let Some(agent) = self
                    .auto_run
                    .agents
                    .iter_mut()
                    .find(|agent| agent.id == *agent_id)
                {
                    agent.preview = Some(truncate_preview(preview));
                }
            }
            OrchestrationEvent::AgentCompleted {
                run_id,
                agent_id,
                success,
            } => {
                self.auto_run.run_id = Some(run_id.clone());
                self.auto_run
                    .mark_agent_completed(run_id, agent_id, *success);
            }
            OrchestrationEvent::GraphReady { run_id, node_count } => {
                self.auto_run.run_id = Some(run_id.clone());
                self.auto_run.total_agents = *node_count;
                self.active_action = Some(format!("Auto graph ready: {node_count} nodes"));
            }
            OrchestrationEvent::NodeQueued {
                run_id, node_id, ..
            } => {
                self.auto_run
                    .upsert_agent(run_id, node_id, node_id.clone(), String::from("agent"));
            }
            OrchestrationEvent::NodeStarted {
                run_id,
                node_id,
                phase,
            } => {
                self.auto_run.mark_phase(run_id, phase.clone());
                self.auto_run
                    .mark_agent_running(run_id, node_id, phase.clone());
                self.active_action = Some(format!("Auto phase: {phase}"));
            }
            OrchestrationEvent::ArtifactProduced {
                run_id,
                node_id,
                title,
                preview,
            } => {
                self.auto_run.run_id = Some(run_id.clone());
                if !self
                    .auto_run
                    .agents
                    .iter()
                    .any(|agent| agent.id == *node_id)
                {
                    let phase = self.auto_run.active_phase.clone().unwrap_or_default();
                    self.auto_run
                        .upsert_agent(run_id, node_id, node_id.clone(), phase);
                }
                self.auto_run.push_artifact_preview(title, preview);
                if let Some(agent) = self
                    .auto_run
                    .agents
                    .iter_mut()
                    .find(|agent| agent.id == *node_id)
                {
                    agent.preview = Some(truncate_preview(&format!("{title}: {preview}")));
                }
            }
            OrchestrationEvent::NodeCompleted {
                run_id,
                node_id,
                success,
            } => {
                self.auto_run.run_id = Some(run_id.clone());
                self.auto_run
                    .mark_agent_completed(run_id, node_id, *success);
            }
            OrchestrationEvent::NodeFailed {
                run_id,
                node_id,
                error,
            } => {
                let phase = self.auto_run.active_phase.clone().unwrap_or_default();
                self.auto_run
                    .upsert_agent(run_id, node_id, node_id.clone(), phase);
                if let Some(agent) = self
                    .auto_run
                    .agents
                    .iter_mut()
                    .find(|agent| agent.id == *node_id)
                {
                    agent.preview = Some(truncate_preview(error));
                }
                self.auto_run.mark_agent_completed(run_id, node_id, false);
            }
            OrchestrationEvent::AutoCompleted {
                run_id,
                success,
                summary,
            } => {
                self.auto_run.finish_run(run_id, summary.clone(), *success);
                self.active_action = None;
                if *success {
                    self.push_log(format!("Auto mode complete: {summary}"));
                } else {
                    self.push_error(format!("Auto mode failed: {summary}"));
                }
            }
        }
    }

    pub fn format_agent_hud_summary(&self) -> String {
        crate::tui::agent_status_feed::format_agent_summary(&self.auto_run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_state_tracks_artifact_preview_for_node() {
        let mut state = AppState::new("test-model".into(), "sandbox".into(), "repo".into());
        state.apply_orchestration_event(&crow_runtime::event::OrchestrationEvent::AutoStarted {
            run_id: "auto-1".into(),
            prompt: "refactor".into(),
            agent_count: 2,
        });
        state.apply_orchestration_event(&crow_runtime::event::OrchestrationEvent::NodeStarted {
            run_id: "auto-1".into(),
            node_id: "explorer-1".into(),
            phase: "Explore".into(),
        });
        state.apply_orchestration_event(
            &crow_runtime::event::OrchestrationEvent::ArtifactProduced {
                run_id: "auto-1".into(),
                node_id: "explorer-1".into(),
                title: "Files".into(),
                preview: "auto.rs and thread_manager.rs".into(),
            },
        );

        let explorer = state
            .auto_run
            .agents
            .iter()
            .find(|agent| agent.id == "explorer-1");
        assert_eq!(
            explorer.and_then(|agent| agent.preview.as_deref()),
            Some("Files: auto.rs and thread_manager.rs")
        );
        assert_eq!(state.auto_run.running_agents, 1);
        assert_eq!(state.auto_run.recent_artifacts.len(), 1);
    }

    #[test]
    fn auto_state_tracks_completion_and_failure_counts() {
        let mut state = AppState::new("test-model".into(), "sandbox".into(), "repo".into());
        state.apply_orchestration_event(&crow_runtime::event::OrchestrationEvent::AutoStarted {
            run_id: "auto-1".into(),
            prompt: "refactor".into(),
            agent_count: 2,
        });
        state.apply_orchestration_event(&crow_runtime::event::OrchestrationEvent::NodeStarted {
            run_id: "auto-1".into(),
            node_id: "executor-1".into(),
            phase: "Execute".into(),
        });
        state.apply_orchestration_event(&crow_runtime::event::OrchestrationEvent::NodeFailed {
            run_id: "auto-1".into(),
            node_id: "executor-1".into(),
            error: "tests failed".into(),
        });

        assert_eq!(state.auto_run.running_agents, 0);
        assert_eq!(state.auto_run.completed_agents, 1);
        assert_eq!(state.auto_run.failed_agents, 1);
        let executor = state
            .auto_run
            .agents
            .iter()
            .find(|agent| agent.id == "executor-1");
        assert_eq!(executor.map(|agent| agent.status.as_str()), Some("Failed"));
    }
}
