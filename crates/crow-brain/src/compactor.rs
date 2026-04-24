use crate::compiler::ChatMessage;
use crate::IntentCompiler;
use anyhow::{Context, Result};
use std::sync::Arc;

/// Codex-style compaction prompt. Creates a structured handoff summary
/// that allows another LLM to seamlessly resume the task.
pub const DEFAULT_COMPACTION_PROMPT: &str = r"You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.

Include:
- Current progress and key decisions made
- Important context, constraints, or user preferences
- What remains to be done (clear next steps)
- Any critical data, examples, or references needed to continue

Be concise, structured, and focused on helping the next LLM seamlessly continue the work.";

pub struct CompactorConfig {
    /// Token threshold to trigger compaction (should be ~80% of context_window)
    pub max_history_tokens: usize,
    /// Total context window size for the model
    pub context_window: usize,
    /// Ratio of context_window at which to trigger compaction (0.0-1.0)
    /// Default: 0.8 (compact when history reaches 80% of context window)
    pub compact_threshold_ratio: f64,
    /// Number of recent turns to preserve exactly during compaction
    pub preservation_turns: usize,
    /// Maximum retries for LLM-based compaction (codex pattern: backoff on failure)
    pub max_retries: usize,
    /// Custom compaction prompt. If None, uses DEFAULT_COMPACTION_PROMPT.
    pub compaction_prompt: Option<String>,
}

impl Default for CompactorConfig {
    fn default() -> Self {
        Self {
            // ~80% of 128K context window (codex pattern: DEFAULT_COMPACT_THRESHOLD)
            max_history_tokens: 80_000,
            context_window: 131_072, // 128K config bounds
            compact_threshold_ratio: 0.8,
            preservation_turns: 4,   // Keep enough recent context for coherent reasoning
            max_retries: 2,          // Retry twice on transient LLM failures
            compaction_prompt: None,
        }
    }
}

impl CompactorConfig {
    /// Create a config with a specific context window size.
    /// The compaction threshold is automatically calculated from the ratio.
    pub fn with_context_window(mut self, context_window: usize) -> Self {
        self.context_window = context_window;
        self.max_history_tokens =
            (context_window as f64 * self.compact_threshold_ratio) as usize;
        self
    }

    /// Set the compaction threshold ratio (0.0-1.0).
    pub fn with_threshold_ratio(mut self, ratio: f64) -> Self {
        self.compact_threshold_ratio = ratio.clamp(0.1, 0.95);
        self.max_history_tokens =
            (self.context_window as f64 * self.compact_threshold_ratio) as usize;
        self
    }

    /// Set a custom compaction prompt.
    pub fn with_compaction_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.compaction_prompt = Some(prompt.into());
        self
    }
}

pub struct Compactor {
    pub config: CompactorConfig,
}

const CLEARED_MARKER: &str = "[Old tool result content cleared]";
const SUMMARY_PREFIX: &str = "[COMPACTED HISTORY SUMMARY]";

impl Compactor {
    pub fn new(config: CompactorConfig) -> Self {
        Self { config }
    }

    /// Rough heuristic for tracking. 1 token ~= 4 chars
    pub fn should_compact(&self, messages: &[ChatMessage]) -> bool {
        let total_chars: usize = messages.iter().map(|m| m.content.len()).sum();
        let estimated_tokens = total_chars / 4;
        estimated_tokens > self.config.max_history_tokens
    }

    /// Approximate token count for a single message (codex utility pattern).
    pub fn approx_token_count(text: &str) -> usize {
        text.len() / 4
    }

    /// Phase 1: Micro-compaction (free, no API call).
    /// Replaces old tool result content with a cleared marker,
    /// preserving message structure. Returns None if nothing to clear.
    pub fn micro_compact(&self, messages: &[ChatMessage]) -> Option<Vec<ChatMessage>> {
        let keep_start = messages
            .len()
            .saturating_sub(self.config.preservation_turns);
        if keep_start == 0 {
            return None;
        }

        let mut modified = false;
        let mut result = Vec::with_capacity(messages.len());

        for (idx, msg) in messages.iter().enumerate() {
            // Only clear old tool-result-like messages outside the preservation window
            if idx < keep_start
                && (msg.content.starts_with("[RECON RESULT]")
                    || msg.content.starts_with("[FILE CONTENTS]"))
                && msg.content != CLEARED_MARKER
            {
                let mut cleared = msg.clone();
                cleared.content = CLEARED_MARKER.to_string();
                result.push(cleared);
                modified = true;
            } else {
                result.push(msg.clone());
            }
        }

        if modified {
            Some(result)
        } else {
            None
        }
    }

    /// Auto-compact: try micro-compaction first, then full LLM summarization.
    /// Includes retry with exponential backoff (codex pattern).
    pub async fn compact(
        &self,
        messages: &[ChatMessage],
        compiler: &Arc<IntentCompiler>,
    ) -> Result<Vec<ChatMessage>> {
        if messages.len() <= self.config.preservation_turns {
            return Ok(messages.to_vec());
        }

        // Phase 1: Try micro-compaction (free)
        if let Some(micro_compacted) = self.micro_compact(messages) {
            if !self.should_compact(&micro_compacted) {
                return Ok(micro_compacted);
            }
            // Micro wasn't enough — fall through to full compaction on micro result
            return self
                .full_compact_with_retry(&micro_compacted, compiler)
                .await;
        }

        // Phase 2: Full LLM summarization with retry
        self.full_compact_with_retry(messages, compiler).await
    }

    /// Full compaction with exponential backoff retry (codex pattern).
    async fn full_compact_with_retry(
        &self,
        messages: &[ChatMessage],
        compiler: &Arc<IntentCompiler>,
    ) -> Result<Vec<ChatMessage>> {
        let mut last_err = None;

        for attempt in 0..=self.config.max_retries {
            match self.full_compact(messages, compiler).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < self.config.max_retries {
                        // Exponential backoff: 500ms, 1s, 2s...
                        let delay = std::time::Duration::from_millis(500 * (1 << attempt));
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Compaction failed after retries")))
    }

    /// Full compaction: summarize old messages via LLM API call.
    /// Preserves user messages with token budget (codex pattern).
    async fn full_compact(
        &self,
        messages: &[ChatMessage],
        compiler: &Arc<IntentCompiler>,
    ) -> Result<Vec<ChatMessage>> {
        let split_idx = messages
            .len()
            .saturating_sub(self.config.preservation_turns);
        let (old_messages, recent_messages) = messages.split_at(split_idx);

        let summary = compiler
            .compile_summary_of_history(old_messages)
            .await
            .context("Failed to run LLM compaction")?;

        // Codex pattern: preserve user messages from old history with token budget
        let user_messages = collect_user_messages(old_messages);

        build_compacted_history(&user_messages, &summary, recent_messages)
    }
}

/// Collect user messages from old history for preservation during compaction.
fn collect_user_messages(messages: &[ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| matches!(m.role, crate::compiler::ChatRole::User))
        .filter(|m| !m.content.starts_with(SUMMARY_PREFIX))
        .filter(|m| !m.content.starts_with("[SYSTEM"))
        .map(|m| m.content.clone())
        .collect()
}

/// Build compacted history (codex pattern):
/// 1. Include truncated user messages (token-budget limited)
/// 2. Summary of old conversation
/// 3. Recent messages preserved exactly
fn build_compacted_history(
    user_messages: &[String],
    summary: &str,
    recent_messages: &[ChatMessage],
) -> Result<Vec<ChatMessage>> {
    const USER_MSG_TOKEN_BUDGET: usize = 20_000;
    let mut next_messages = Vec::new();

    // Include recent user messages from old history (token-budget limited, codex pattern)
    let mut remaining_budget = USER_MSG_TOKEN_BUDGET;
    let mut selected: Vec<&str> = Vec::new();
    for msg in user_messages.iter().rev() {
        let tokens = Compactor::approx_token_count(msg);
        if tokens <= remaining_budget {
            selected.push(msg);
            remaining_budget = remaining_budget.saturating_sub(tokens);
        } else {
            break;
        }
    }
    selected.reverse();
    for msg in selected {
        next_messages.push(ChatMessage::user(msg));
    }

    // Add compaction summary
    next_messages.push(ChatMessage::assistant(format!(
        "{SUMMARY_PREFIX}\n{summary}"
    )));

    // Preserve recent messages exactly
    next_messages.extend_from_slice(recent_messages);

    Ok(next_messages)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::compiler::ChatMessage;

    #[test]
    fn micro_compact_clears_old_recon() {
        let config = CompactorConfig {
            preservation_turns: 1,
            ..Default::default()
        };
        let compactor = Compactor::new(config);
        let messages = vec![
            ChatMessage::assistant("[RECON RESULT] some old content"),
            ChatMessage::user("latest question"),
        ];
        let result = compactor.micro_compact(&messages).unwrap();
        assert_eq!(result[0].content, CLEARED_MARKER);
        assert_eq!(result[1].content, "latest question");
    }

    #[test]
    fn micro_compact_clears_file_contents() {
        let config = CompactorConfig {
            preservation_turns: 1,
            ..Default::default()
        };
        let compactor = Compactor::new(config);
        let messages = vec![
            ChatMessage::assistant("[FILE CONTENTS] big file dump"),
            ChatMessage::user("question about file"),
        ];
        let result = compactor.micro_compact(&messages).unwrap();
        assert_eq!(result[0].content, CLEARED_MARKER);
    }

    #[test]
    fn approx_token_count_works() {
        assert_eq!(Compactor::approx_token_count("hello world!"), 3);
        assert_eq!(Compactor::approx_token_count(""), 0);
    }

    #[test]
    fn should_compact_respects_threshold() {
        let config = CompactorConfig {
            max_history_tokens: 10,
            ..Default::default()
        };
        let compactor = Compactor::new(config);
        // 44 chars / 4 = 11 tokens > 10 threshold
        let messages = vec![ChatMessage::user(
            "a]".repeat(22), // 44 chars
        )];
        assert!(compactor.should_compact(&messages));
    }

    #[test]
    fn collect_user_messages_filters_system_and_summaries() {
        let messages = vec![
            ChatMessage::user("real question"),
            ChatMessage::user("[SYSTEM COMPACTION REQUEST] blah"),
            ChatMessage::user("[COMPACTED HISTORY SUMMARY]\nold stuff"),
            ChatMessage::user("another real question"),
        ];
        let collected = collect_user_messages(&messages);
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0], "real question");
        assert_eq!(collected[1], "another real question");
    }
}
