use crow_patch::IntentPlan;

// ── Structured Protocol Layer (SQ/EQ Pattern) ──────────────────────

/// Turn lifecycle events — always delivered, represent major phase transitions.
#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// A new turn has begun.
    Started { turn_id: String },
    /// The turn completed (success or failure).
    Completed {
        turn_id: String,
        success: bool,
        token_usage: Option<TokenUsageSummary>,
    },
    /// The turn was aborted by the user.
    Aborted { turn_id: String, reason: String },
    /// The turn transitioned to a new phase.
    PhaseChanged { turn_id: String, phase: TurnPhase },
}

/// Phases of a turn lifecycle — used for status bar and telemetry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnPhase {
    Materializing,
    BuildingRepoMap,
    Compacting,
    EpistemicLoop { step: u32, max_steps: u32 },
    CruciblePreflight,
    CrucibleVerification { attempt: u32 },
    Applying,
    Complete,
}

impl std::fmt::Display for TurnPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Materializing => write!(f, "Materializing"),
            Self::BuildingRepoMap => write!(f, "Building Repo Map"),
            Self::Compacting => write!(f, "Compacting"),
            Self::EpistemicLoop { step, max_steps } => write!(f, "Epistemic [{step}/{max_steps}]"),
            Self::CruciblePreflight => write!(f, "Preflight"),
            Self::CrucibleVerification { attempt } => write!(f, "Crucible [attempt {attempt}]"),
            Self::Applying => write!(f, "Applying"),
            Self::Complete => write!(f, "Complete"),
        }
    }
}

/// Token usage summary attached to turn completion events.
#[derive(Debug, Clone, Default)]
pub struct TokenUsageSummary {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub context_window: u32,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Turn(TurnEvent),
    Thinking(u32, u32),
    StreamChunk(String),
    ActionStart(String),
    ActionComplete(String),
    PlanSubmitted(IntentPlan),
    CruciblePreflight(String),
    ReadFiles(Vec<String>),
    ReconStart(String),
    DelegateStart(String, String),
    DelegateComplete(String, bool),
    StateChanged {
        from: String,
        to: String,
    },
    Retrying {
        attempt: u32,
        max_attempts: u32,
        reason: String,
    },
    Error(String),
    Log(String),
    Markdown(String),
    Compacting {
        active: bool,
    },
    ToolProgress {
        tool_id: String,
        message: String,
    },
    TokenUsage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        context_window: u32,
    },
}

/// A generic handler that allows the epistemic engine to stream events outward.
pub trait EventHandler: Send {
    fn handle_event(&mut self, event: AgentEvent);

    /// Returns true if the user has requested cancellation of the current turn.
    /// The epistemic loop checks this at each iteration boundary.
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Broad events sent from the autonomous runtime to the frontend/TUI.
pub enum EngineEvent {
    AgentEvent(AgentEvent),
    SessionComplete,
    /// Turn completed with success flag and optional timing data.
    TurnComplete(bool, Option<TurnTimingSummary>),
    SwarmStarted(String, String),
    SwarmComplete(String, bool),
}

/// Compact summary of turn timing for display in the TUI.
#[derive(Debug, Clone)]
pub struct TurnTimingSummary {
    /// Total wall-clock time.
    pub total_ms: u64,
    /// Time spent in tool execution.
    pub tool_ms: u64,
    /// Number of LLM API calls.
    pub llm_calls: u32,
    /// Number of compactions.
    pub compactions: u32,
    /// Time to first token.
    pub ttft_ms: Option<u64>,
}

pub struct ChannelEventHandler {
    tx: tokio::sync::mpsc::UnboundedSender<EngineEvent>,
    cancellation: Option<crate::cancel::CancellationToken>,
}

impl ChannelEventHandler {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<EngineEvent>) -> Self {
        Self {
            tx,
            cancellation: None,
        }
    }

    pub fn with_cancellation(
        tx: tokio::sync::mpsc::UnboundedSender<EngineEvent>,
        token: crate::cancel::CancellationToken,
    ) -> Self {
        Self {
            tx,
            cancellation: Some(token),
        }
    }
}

impl EventHandler for ChannelEventHandler {
    fn handle_event(&mut self, event: AgentEvent) {
        let _ = self.tx.send(EngineEvent::AgentEvent(event));
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .map(crate::cancel::CancellationToken::is_cancelled)
            .unwrap_or(false)
    }
}
