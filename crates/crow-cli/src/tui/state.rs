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
        self.history.push(Box::new(history_cell::ToolCallCell::from_completed(
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
    pub fn push_summary(
        &mut self,
        tool_count: usize,
        duration_ms: u64,
        tokens: u64,
    ) {
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
            status_message: None,
            status_message_timeout: None,
            status_indicator: None,
            show_shortcuts_overlay: false,
            quit_hint_until: None,
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
}
