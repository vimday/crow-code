//! Immutable per-turn context snapshot (Codex `TurnContext` pattern).
//!
//! Each agent turn creates a `TurnContext` at the start, capturing all
//! configuration and dependencies needed for the turn's lifecycle.
//! This replaces scattered turn state and ensures no mid-turn mutation
//! of configuration can corrupt an in-progress turn.
//!
//! The struct is intentionally `Clone`-friendly via `Arc` wrapping of
//! heavy resources, so it can be cheaply shared across parallel tool
//! calls and subagent tasks within the same turn.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::budget::ModelBudget;

/// Immutable snapshot of all state needed for a single agent turn.
///
/// Created once at turn start and threaded through the entire turn
/// lifecycle — LLM calls, tool execution, compaction, event emission.
/// Modeled after Codex's `TurnContext` from `session/turn_context.rs`.
///
/// # Design Rationale
///
/// Before `TurnContext`, turn configuration was spread across:
/// - `TurnContext` (now unified — previously `TurnConfig` in agent_loop.rs)
/// - `ModelBudget` (budget.rs) — context limits
/// - `AppState` (TUI) — model name, streaming state
/// - Bare function parameters — workspace root, cancel token
///
/// This caused two problems:
/// 1. Configuration could mutate mid-turn (e.g., user switches model
///    while a turn is executing)
/// 2. Adding new per-turn state required threading another parameter
///    through the entire call chain
///
/// `TurnContext` solves both by snapshotting everything once at turn start.
#[derive(Clone)]
pub struct TurnContext {
    /// Unique identifier for this turn (UUID v4).
    pub turn_id: String,

    /// Model identifier (e.g., "claude-sonnet-4-20250514").
    /// Frozen at turn start — model switches mid-turn are ignored.
    pub model: String,

    /// Provider identifier (e.g., "anthropic", "openai").
    pub provider: String,

    /// The intent compiler / LLM client for this turn.
    pub compiler: Arc<crow_brain::IntentCompiler>,

    /// Workspace root path.
    pub workspace_root: PathBuf,

    /// Context budget snapshot — frozen at turn start based on the model.
    pub budget: ModelBudget,

    /// Tool registry for this turn.
    pub tool_registry: Arc<crow_tools::ToolRegistry>,

    /// Permission enforcer for this turn.
    pub permissions: Arc<crow_tools::PermissionEnforcer>,

    /// File state store (tracks modified files within a turn).
    pub file_state: Arc<crow_tools::FileStateStore>,

    /// Background process manager.
    pub background_manager: Arc<crow_tools::BackgroundProcessManager>,

    /// Optional subagent delegator for spawning child agents.
    pub subagent_delegator: Option<Arc<dyn crow_tools::SubagentDelegator>>,

    /// Cancellation token for this turn. Child tokens can be derived
    /// for individual tool calls.
    pub cancel_token: CancellationToken,

    /// Maximum agent loop iterations (default: 40).
    pub max_steps: usize,

    /// When the turn started. Used for TTFT/TTFM tracking.
    pub started_at: Instant,

    /// Reasoning effort level (e.g., "low", "medium", "high").
    /// Maps to Codex's `reasoning_effort` field.
    pub reasoning_effort: Option<String>,

    /// Turn timing state for TTFT/TTFM/duration tracking.
    pub timing: Arc<crate::turn_timing::TurnTimingState>,

    /// Agent role for this turn (controls permissions, prompt overlay, limits).
    pub role: crate::role::AgentRole,

    /// Turn-level diff tracker (Codex TurnDiffTracker pattern).
    /// Snapshots files before write-tools modify them, then computes
    /// aggregated unified diffs at turn completion for the `/diff` command.
    /// Wrapped in `Mutex` so tool executors can snapshot from async contexts.
    pub diff_tracker: Arc<Mutex<crate::turn_diff::TurnDiffTracker>>,

    /// Optional MCP manager for MCP tool calls.
    pub mcp_manager: Option<Arc<crate::mcp::McpManager>>,
}

impl TurnContext {
    /// Create a new `TurnContext` with the given model and compiler.
    ///
    /// Use `TurnContextBuilder` for ergonomic construction with defaults.
    pub fn builder() -> TurnContextBuilder {
        TurnContextBuilder::default()
    }

    /// Derive a child cancellation token for a tool call.
    pub fn child_cancel_token(&self) -> CancellationToken {
        self.cancel_token.child_token()
    }

    /// Check if this turn has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Get the model's context window size in tokens.
    pub fn model_context_window(&self) -> usize {
        self.budget.max_context_bytes / 4 // approximate: 4 bytes per token
    }

    /// Elapsed time since turn start.
    pub fn elapsed(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    /// Generate the compaction prompt string (Codex compact_prompt pattern).
    ///
    /// Returns a structured prompt that instructs the LLM to produce
    /// a context-checkpoint summary for handoff to the next turn.
    pub fn compact_prompt(&self) -> String {
        format!(
            "You are performing a CONTEXT CHECKPOINT COMPACTION for model '{model}' \
            (turn {turn_id}, elapsed {elapsed:.1}s).\n\n\
            Create a handoff summary for the next LLM that will resume the task.\n\n\
            Include:\n\
            - Current progress and key decisions made\n\
            - Important context, constraints, or user preferences\n\
            - What remains to be done (clear next steps)\n\
            - Any critical data, examples, or references needed to continue\n\n\
            Be concise, structured, and focused on helping the next LLM seamlessly continue the work.",
            model = self.model,
            turn_id = &self.turn_id[..8.min(self.turn_id.len())],
            elapsed = self.elapsed().as_secs_f64(),
        )
    }
}

/// Builder for `TurnContext` — provides sensible defaults for testing
/// and ergonomic construction in production code.
#[derive(Default)]
pub struct TurnContextBuilder {
    model: Option<String>,
    provider: Option<String>,
    compiler: Option<Arc<crow_brain::IntentCompiler>>,
    workspace_root: Option<PathBuf>,
    budget: Option<ModelBudget>,
    tool_registry: Option<Arc<crow_tools::ToolRegistry>>,
    permissions: Option<Arc<crow_tools::PermissionEnforcer>>,
    file_state: Option<Arc<crow_tools::FileStateStore>>,
    background_manager: Option<Arc<crow_tools::BackgroundProcessManager>>,
    subagent_delegator: Option<Arc<dyn crow_tools::SubagentDelegator>>,
    cancel_token: Option<CancellationToken>,
    max_steps: Option<usize>,
    reasoning_effort: Option<String>,
    role: Option<crate::role::AgentRole>,
    mcp_manager: Option<Arc<crate::mcp::McpManager>>,
}


impl TurnContextBuilder {
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn compiler(mut self, compiler: Arc<crow_brain::IntentCompiler>) -> Self {
        self.compiler = Some(compiler);
        self
    }

    pub fn workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    pub fn budget(mut self, budget: ModelBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    pub fn tool_registry(mut self, registry: Arc<crow_tools::ToolRegistry>) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    pub fn permissions(mut self, perms: Arc<crow_tools::PermissionEnforcer>) -> Self {
        self.permissions = Some(perms);
        self
    }

    pub fn file_state(mut self, fs: Arc<crow_tools::FileStateStore>) -> Self {
        self.file_state = Some(fs);
        self
    }

    pub fn background_manager(
        mut self,
        bgm: Arc<crow_tools::BackgroundProcessManager>,
    ) -> Self {
        self.background_manager = Some(bgm);
        self
    }

    pub fn subagent_delegator(
        mut self,
        delegator: Arc<dyn crow_tools::SubagentDelegator>,
    ) -> Self {
        self.subagent_delegator = Some(delegator);
        self
    }

    pub fn cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }

    pub fn max_steps(mut self, steps: usize) -> Self {
        self.max_steps = Some(steps);
        self
    }

    pub fn reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    pub fn role(mut self, role: crate::role::AgentRole) -> Self {
        self.role = Some(role);
        self
    }

    pub fn mcp_manager(mut self, mcp: Arc<crate::mcp::McpManager>) -> Self {
        self.mcp_manager = Some(mcp);
        self
    }

    /// Build the `TurnContext`, deriving defaults where possible.
    ///
    /// # Errors
    ///
    /// Returns an error if `compiler`, `tool_registry`, `permissions`,
    /// `file_state`, or `background_manager` are not set.
    pub fn build(self) -> Result<TurnContext, String> {
        let model = self.model.unwrap_or_else(|| "unknown".to_string());
        let budget = self
            .budget
            .unwrap_or_else(|| ModelBudget::for_model(&model));

        Ok(TurnContext {
            turn_id: uuid::Uuid::new_v4().to_string(),
            model,
            provider: self.provider.unwrap_or_else(|| "unknown".to_string()),
            compiler: self.compiler.ok_or("TurnContext requires a compiler")?,
            workspace_root: self
                .workspace_root
                .unwrap_or_else(|| PathBuf::from(".")),
            budget,
            tool_registry: self
                .tool_registry
                .ok_or("TurnContext requires a tool_registry")?,
            permissions: self
                .permissions
                .ok_or("TurnContext requires permissions")?,
            file_state: self
                .file_state
                .ok_or("TurnContext requires file_state")?,
            background_manager: self
                .background_manager
                .ok_or("TurnContext requires background_manager")?,
            subagent_delegator: self.subagent_delegator,
            cancel_token: self
                .cancel_token
                .unwrap_or_default(),
            max_steps: self.max_steps.unwrap_or(40),
            started_at: Instant::now(),
            reasoning_effort: self.reasoning_effort,
            timing: Arc::new(crate::turn_timing::TurnTimingState::new()),
            role: self.role.unwrap_or_default(),
            diff_tracker: Arc::new(Mutex::new(crate::turn_diff::TurnDiffTracker::new())),
            mcp_manager: self.mcp_manager,
        })
    }
}

impl std::fmt::Debug for TurnContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnContext")
            .field("turn_id", &self.turn_id)
            .field("model", &self.model)
            .field("provider", &self.provider)
            .field("workspace_root", &self.workspace_root)
            .field("budget", &self.budget)
            .field("max_steps", &self.max_steps)
            .field("started_at", &self.started_at)
            .field("reasoning_effort", &self.reasoning_effort)
            .finish_non_exhaustive()
    }
}
