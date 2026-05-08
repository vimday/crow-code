//! Single source of truth for context budget constants.
//!
//! Both `CrowConfig` (config-time clamp) and `ConversationManager`
//! (runtime enforcement) derive their limits from these values,
//! ensuring the two layers can never silently drift apart.
//!
//! For model-aware budgets, use [`for_model`] which queries the model
//! registry for the context window size and scales limits accordingly.

/// Hard cap on the total LLM context envelope (system + conversation).
/// This is the **fallback default** for unknown models.
pub const MAX_CONTEXT_BYTES: usize = 768 * 1024; // 768 KB

/// Minimum bytes reserved for conversational history.
/// The system anchor (repo map + instructions) may never consume
/// more than `MAX_CONTEXT_BYTES - MIN_HISTORY_RESERVE`.
pub const MIN_HISTORY_RESERVE: usize = 64 * 1024; // 64 KB

/// Maximum bytes available for system messages (repo map + static prompts).
/// Derived: `MAX_CONTEXT_BYTES - MIN_HISTORY_RESERVE`.
pub const MAX_SYSTEM_BYTES: usize = MAX_CONTEXT_BYTES - MIN_HISTORY_RESERVE; // 704 KB

/// Maximum number of conversation turns (user + assistant messages)
/// before hard eviction kicks in.
pub const MAX_HISTORY_TURNS: usize = 30;

/// Model-aware context budget.
///
/// Scales `max_context_bytes` and `max_system_bytes` based on the model's
/// context window from the model registry. Unknown models fall back to the
/// default 768KB budget.
#[derive(Debug, Clone, Copy)]
pub struct ModelBudget {
    /// Total context envelope in bytes.
    pub max_context_bytes: usize,
    /// Maximum bytes for system messages.
    pub max_system_bytes: usize,
    /// Maximum conversation turns.
    pub max_history_turns: usize,
}

impl Default for ModelBudget {
    fn default() -> Self {
        Self {
            max_context_bytes: MAX_CONTEXT_BYTES,
            max_system_bytes: MAX_SYSTEM_BYTES,
            max_history_turns: MAX_HISTORY_TURNS,
        }
    }
}

impl ModelBudget {
    /// Derive a budget from the model's context window size.
    ///
    /// For a 1M-token model (GPT-5.x, Claude Opus 4.7), the context
    /// window is ~4MB, so the budget scales up accordingly. For unknown
    /// models, falls back to the default 768KB budget.
    pub fn for_model(model: &str) -> Self {
        let context_bytes =
            crow_brain::model_registry::context_window_bytes(model, MAX_CONTEXT_BYTES);

        // Scale turns proportionally: 1M-token models get more turns
        let turns = if context_bytes > MAX_CONTEXT_BYTES {
            let scale = context_bytes as f64 / MAX_CONTEXT_BYTES as f64;
            (MAX_HISTORY_TURNS as f64 * scale.sqrt()) as usize // sqrt scaling to avoid absurd turn counts
        } else {
            MAX_HISTORY_TURNS
        };

        Self {
            max_context_bytes: context_bytes,
            max_system_bytes: context_bytes.saturating_sub(MIN_HISTORY_RESERVE),
            max_history_turns: turns.min(200), // hard cap at 200 turns
        }
    }
}
