use crate::compiler::ChatMessage;
use crate::IntentCompiler;
use anyhow::{Context, Result};
use std::sync::Arc;

pub struct CompactorConfig {
    /// Token threshold to trigger compaction (should be ~80% of context_window)
    pub max_history_tokens: usize,
    /// Total context window size for the model
    pub context_window: usize,
    /// Number of recent turns to preserve exactly during compaction
    pub preservation_turns: usize,
}

impl Default for CompactorConfig {
    fn default() -> Self {
        Self {
            // ~80% of 128K context window (yomi pattern: DEFAULT_COMPACT_THRESHOLD)
            // Previous 16K value was far too aggressive — system prompt alone
            // exceeds 16K tokens causing compaction on the very first message.
            max_history_tokens: 80_000,
            context_window: 131_072, // 128K config bounds
            preservation_turns: 4,   // Keep enough recent context for coherent reasoning
        }
    }
}

pub struct Compactor {
    pub config: CompactorConfig,
}

const CLEARED_MARKER: &str = "[Old tool result content cleared]";

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
                && msg.content.starts_with("[RECON RESULT]")
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
            return self.full_compact(&micro_compacted, compiler).await;
        }

        // Phase 2: Full LLM summarization
        self.full_compact(messages, compiler).await
    }

    /// Full compaction: summarize old messages via LLM API call.
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

        let compressed_msg =
            ChatMessage::assistant(format!("[COMPACTED HISTORY SUMMARY]\n{summary}"));

        let mut next_messages = Vec::with_capacity(recent_messages.len() + 1);
        next_messages.push(compressed_msg);
        next_messages.extend_from_slice(recent_messages);

        Ok(next_messages)
    }
}
