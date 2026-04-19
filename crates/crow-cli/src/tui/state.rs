use crate::event::{AgentEvent, ViewMode};
use crow_patch::SnapshotId;
use std::time::Instant;

// ── TUI Message Bus ──────────────────────────────────────────────────────────

pub enum TuiMessage {
    AgentEvent(AgentEvent),
    TurnComplete(bool),
    SessionComplete,
    SwarmStarted(String, String),
    SwarmComplete(String, bool),
    Tick,
}

// ── Typed History Cells (Codex-style) ────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellKind {
    /// User-authored prompt. Rendered with `› ` prefix and subtle background.
    User,
    /// Agent markdown response. Rendered with `• ` prefix.
    AgentMessage,
    /// Evidence trace (file reads, recon). Dim, secondary info.
    Evidence,
    /// Tool/action execution trace.
    Action,
    /// Final verdict for a turn.
    Result,
    /// System-level informational log.
    Log,
    /// Error.
    Error,
}

#[derive(Debug, Clone)]
pub struct Cell {
    pub kind: CellKind,
    pub payload: String,
}

// ── Cancellation Token ───────────────────────────────────────────────────────

/// Shared cancellation flag for interrupting a running agent turn.
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Reset the cancellation flag for a new turn.
    /// Without this, a cancelled turn leaves the flag permanently set,
    /// causing all subsequent turns to see a stale cancellation.
    pub fn reset(&self) {
        self.inner.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

// ── App State ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum ApprovalState {
    None,
    PendingCommand(String),
}

#[derive(Clone, PartialEq, Eq)]
pub enum OverlayState {
    None,
    CommandPalette { query: String, selected_idx: usize },
}

pub struct AppState {
    // Composer Advanced State
    pub composer: String,
    pub composer_cursor: usize,
    pub input_history: Vec<String>,
    pub input_history_idx: Option<usize>,

    // View
    pub view_mode: ViewMode,
    pub history: Vec<Cell>,
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

    // TUI Modal Overlay
    pub overlay_state: OverlayState,

    // Quit state (Codex-style: Ctrl+C twice to quit)
    pub last_ctrl_c: Option<Instant>,

    // Status Substrate Context
    pub model_info: String,
    pub write_mode: String,
    pub workspace_name: String,
    pub git_branch: String,
    pub is_dirty: bool,
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
            overlay_state: OverlayState::None,
            last_ctrl_c: None,
            model_info,
            write_mode,
            workspace_name,
            git_branch: "detecting...".into(),
            is_dirty: false,
        }
    }

    pub fn is_task_running(&self) -> bool {
        self.active_action.is_some()
    }
}

pub fn get_palette_commands(query: &str) -> Vec<(&'static str, &'static str)> {
    let all = vec![
        ("/help", "Show manual"),
        ("/status", "Print system status"),
        ("/clear", "Clear history"),
        ("/model", "Switch LLM Model"),
        ("/view", "Swap Lens Mode (focus|evidence|audit)"),
    ];
    if query == "/" || query.is_empty() {
        all
    } else {
        all.into_iter()
            .filter(|(cmd, _)| cmd.starts_with(query))
            .collect()
    }
}
