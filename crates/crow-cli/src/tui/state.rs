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
#[derive(Clone)]
pub struct CancellationToken {
    inner: std::sync::Arc<arc_swap::ArcSwap<tokio_util::sync::CancellationToken>>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self {
            inner: std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
                tokio_util::sync::CancellationToken::new(),
            ))),
        }
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.load().cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load().is_cancelled()
    }

    /// Retrieve the underlying tokio cancellation token for native loop awaiting.
    pub fn runtime_token(&self) -> tokio_util::sync::CancellationToken {
        (**self.inner.load()).clone()
    }

    /// Safely rotation mechanism utilizing ArcSwap.
    /// If the token is already canceled, atomically spawn and swap in a fresh token
    /// allowing legacy listeners to gracefully fall off rather than deadlocking.
    pub fn reset_if_cancelled(&self) {
        if self.is_cancelled() {
            self.inner.store(std::sync::Arc::new(tokio_util::sync::CancellationToken::new()));
        }
    }

    pub fn force_reset(&self) {
        self.inner.store(std::sync::Arc::new(tokio_util::sync::CancellationToken::new()));
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

            last_ctrl_c: None,
            model_info,
            write_mode,
            workspace_name,
            git_branch: "detecting...".into(),
            is_dirty: false,
            focus: Focus::Composer,
            stream_state: crate::render::MarkdownStreamState::default(),
        }
    }

    pub fn is_task_running(&self) -> bool {
        self.active_action.is_some()
    }
}

pub fn get_palette_commands(query: &str) -> Vec<(String, String)> {
    if query.starts_with('!') {
        let all = vec![
            ("!cargo check", "Run cargo check"),
            ("!cargo test", "Run cargo test"),
            ("!cargo build", "Run cargo build"),
            ("!git status", "Check git status"),
            ("!git diff", "Show git diff"),
            ("!git add .", "Stage all changes"),
            ("!ls -la", "List directory contents"),
            ("!pwd", "Print working directory"),
        ];

        let trimmed_query = query.trim_end();
        if trimmed_query == "!" || trimmed_query.is_empty() {
            return all
                .into_iter()
                .map(|(c, d)| (c.to_string(), d.to_string()))
                .collect();
        } else {
            return all
                .into_iter()
                .filter(|(cmd, _)| cmd.starts_with(trimmed_query))
                .map(|(c, d)| (c.to_string(), d.to_string()))
                .collect();
        }
    }

    let all = vec![
        ("/help", "Show manual"),
        ("/status", "Print system status"),
        ("/clear", "Clear history"),
        ("/model", "Switch LLM Model"),
        ("/view", "Swap Lens Mode (focus|evidence|audit)"),
        ("/swarm", "Launch background sub-agent swarm"),
        ("/compact", "Force context compaction"),
        ("/session list", "List saved sessions"),
        ("/session resume", "Resume a saved session"),
    ];
    if query == "/" || query.is_empty() {
        all.into_iter()
            .map(|(c, d)| (c.to_string(), d.to_string()))
            .collect()
    } else {
        all.into_iter()
            .filter(|(cmd, _)| cmd.starts_with(query))
            .map(|(c, d)| (c.to_string(), d.to_string()))
            .collect()
    }
}
