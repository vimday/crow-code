//! Single source of truth for context budget constants.
//!
//! Both `CrowConfig` (config-time clamp) and `ConversationManager`
//! (runtime enforcement) derive their limits from these values,
//! ensuring the two layers can never silently drift apart.

/// Hard cap on the total LLM context envelope (system + conversation).
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
