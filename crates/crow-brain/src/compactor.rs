use crate::compiler::{ChatMessage, ChatRole};
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
            preservation_turns: 4, // Keep enough recent context for coherent reasoning
            max_retries: 2,        // Retry twice on transient LLM failures
            compaction_prompt: None,
        }
    }
}

impl CompactorConfig {
    /// Create a config with a specific context window size.
    /// The compaction threshold is automatically calculated from the ratio.
    pub fn with_context_window(mut self, context_window: usize) -> Self {
        self.context_window = context_window;
        self.max_history_tokens = (context_window as f64 * self.compact_threshold_ratio) as usize;
        self
    }

    /// Create a config auto-sized for a specific model.
    ///
    /// Uses the model registry to look up the context window and sets
    /// thresholds accordingly. Falls back to defaults for unknown models.
    pub fn for_model(model: &str) -> Self {
        let base = Self::default();
        if let Some(limit) = crate::model_registry::model_token_limit(model) {
            base.with_context_window(limit.context_window_tokens as usize)
        } else {
            base
        }
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

/// Preamble injected into structured compaction summaries (claw-code pattern).
const COMPACT_CONTINUATION_PREAMBLE: &str =
    "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\n";

/// Instruction appended when recent messages are preserved.
const COMPACT_RECENT_MESSAGES_NOTE: &str = "Recent messages are preserved verbatim.";

/// Instruction to resume without acknowledging the summary.
const COMPACT_DIRECT_RESUME: &str = "Continue the conversation from where it left off without asking the user any further questions. Resume directly — do not acknowledge the summary, do not recap what was happening, and do not preface with continuation text.";

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
    /// preserving message structure. Clears ALL tool-result-like
    /// messages outside the preservation window (yomi pattern).
    /// Returns None if nothing to clear.
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
            // Clear old messages outside the preservation window that contain
            // tool output (identified by role or content prefix patterns).
            // This covers all recon results, file contents, and any other
            // tool output that bloats the context.
            let is_clearable = idx < keep_start
                && msg.content != CLEARED_MARKER
                && (msg.role == ChatRole::Tool
                    || msg.content.starts_with("[RECON RESULT]")
                    || msg.content.starts_with("[FILE CONTENTS]")
                    || msg.content.starts_with("[TOOL OUTPUT]"));

            if is_clearable {
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

    /// Auto-compact: try micro-compaction first, then structured local
    /// summarization, then full LLM summarization.
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
        let working = if let Some(micro_compacted) = self.micro_compact(messages) {
            if !self.should_compact(&micro_compacted) {
                return Ok(micro_compacted);
            }
            micro_compacted
        } else {
            messages.to_vec()
        };

        // Phase 1.5: Structured local compaction (free, no API call)
        // Extracts key files, pending work, tool usage, and timeline
        // from the compacted-away messages. This is the claw-code pattern.
        let local_result = self.structured_local_compact(&working);
        if !self.should_compact(&local_result) {
            return Ok(local_result);
        }

        // Phase 2: Full LLM summarization with retry (expensive)
        self.full_compact_with_retry(&local_result, compiler).await
    }

    /// Structured local compaction (claw-code pattern).
    ///
    /// Summarizes older messages into a structured summary without an API call.
    /// Extracts key files, pending work, tool usage, and timeline.
    /// Uses the tool-pair boundary guard to avoid splitting tool call/result pairs.
    pub fn structured_local_compact(&self, messages: &[ChatMessage]) -> Vec<ChatMessage> {
        let split_idx = self.find_safe_split_point(messages);
        if split_idx == 0 {
            return messages.to_vec();
        }

        let (old, recent) = messages.split_at(split_idx);
        let summary = build_structured_summary(old);
        let continuation = format_continuation_message(&summary, !recent.is_empty());

        let mut result = Vec::with_capacity(1 + recent.len());
        result.push(ChatMessage::user(continuation));
        result.extend_from_slice(recent);
        result
    }

    /// Find a safe split point that respects tool-pair boundaries.
    ///
    /// Walks backward from the naive split point to ensure we never split
    /// an Assistant(tool_calls) / Tool(tool_result) pair. This prevents
    /// API 400 errors from orphaned tool messages (claw-code regression fix).
    fn find_safe_split_point(&self, messages: &[ChatMessage]) -> usize {
        let naive_split = messages
            .len()
            .saturating_sub(self.config.preservation_turns);

        if naive_split == 0 {
            return 0;
        }

        let mut k = naive_split;
        // Walk backward if the first preserved message is a Tool role
        // (orphaned tool result), ensuring its paired assistant is preserved too.
        while k > 0 {
            if messages[k].role != ChatRole::Tool {
                break;
            }
            // Check if preceding message is an assistant with tool_calls
            if k > 0
                && messages[k - 1].role == ChatRole::Assistant
                && messages[k - 1].tool_calls.is_some()
            {
                k -= 1; // Include the assistant too
                break;
            }
            k -= 1;
        }
        k
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

// ─── Structured Local Summarization (Claw-Code Pattern) ─────────────

/// Build a structured summary from compacted-away messages.
///
/// Extracts key metadata without an LLM call:
/// - Message counts by role
/// - Tool names mentioned
/// - Recent user requests
/// - Pending work (TODO/NEXT/PENDING keywords)
/// - Key files referenced (paths with code extensions)
/// - Timeline of message roles and truncated content
fn build_structured_summary(messages: &[ChatMessage]) -> String {
    let user_count = messages.iter().filter(|m| m.role == ChatRole::User).count();
    let assistant_count = messages
        .iter()
        .filter(|m| m.role == ChatRole::Assistant)
        .count();
    let tool_count = messages.iter().filter(|m| m.role == ChatRole::Tool).count();

    // Collect tool names from tool_calls
    let mut tool_names: Vec<String> = messages
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flat_map(|tcs| tcs.iter().map(|tc| tc.name.clone()))
        .collect();
    tool_names.sort();
    tool_names.dedup();

    // Collect recent user requests (last 3)
    let recent_user_requests: Vec<String> = messages
        .iter()
        .filter(|m| m.role == ChatRole::User)
        .rev()
        .take(3)
        .map(|m| truncate_summary(&m.content, 160))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    // Infer pending work
    let pending_work: Vec<String> = messages
        .iter()
        .rev()
        .filter(|m| {
            let lower = m.content.to_ascii_lowercase();
            lower.contains("todo")
                || lower.contains("next")
                || lower.contains("pending")
                || lower.contains("follow up")
                || lower.contains("remaining")
        })
        .take(3)
        .map(|m| truncate_summary(&m.content, 160))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    // Extract key files
    let key_files = collect_key_files(messages);

    // Build summary
    let mut lines = vec![
        "<summary>".to_string(),
        "Conversation summary:".to_string(),
        format!(
            "- Scope: {} earlier messages compacted (user={user_count}, assistant={assistant_count}, tool={tool_count}).",
            messages.len()
        ),
    ];

    if !tool_names.is_empty() {
        lines.push(format!("- Tools mentioned: {}.", tool_names.join(", ")));
    }

    if !recent_user_requests.is_empty() {
        lines.push("- Recent user requests:".to_string());
        lines.extend(recent_user_requests.iter().map(|r| format!("  - {r}")));
    }

    if !pending_work.is_empty() {
        lines.push("- Pending work:".to_string());
        lines.extend(pending_work.iter().map(|w| format!("  - {w}")));
    }

    if !key_files.is_empty() {
        lines.push(format!("- Key files referenced: {}.", key_files.join(", ")));
    }

    // Timeline
    lines.push("- Key timeline:".to_string());
    for msg in messages {
        let role = match msg.role {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        };
        let content = truncate_summary(&msg.content, 120);
        lines.push(format!("  - {role}: {content}"));
    }
    lines.push("</summary>".to_string());

    lines.join("\n")
}

/// Format a structured compaction summary into a continuation message.
fn format_continuation_message(summary: &str, has_recent: bool) -> String {
    let mut base = format!("{COMPACT_CONTINUATION_PREAMBLE}{summary}");
    if has_recent {
        base.push_str("\n\n");
        base.push_str(COMPACT_RECENT_MESSAGES_NOTE);
    }
    base.push('\n');
    base.push_str(COMPACT_DIRECT_RESUME);
    base
}

/// Extract file path candidates from messages (claw-code pattern).
fn collect_key_files(messages: &[ChatMessage]) -> Vec<String> {
    let mut files: Vec<String> = messages
        .iter()
        .flat_map(|m| extract_file_candidates(&m.content))
        .collect();
    files.sort();
    files.dedup();
    files.into_iter().take(8).collect()
}

/// Extract file path candidates from text content.
fn extract_file_candidates(content: &str) -> Vec<String> {
    let interesting_extensions = ["rs", "ts", "tsx", "js", "json", "md", "py", "go", "toml"];

    content
        .split_whitespace()
        .filter_map(|token| {
            let candidate = token.trim_matches(|c: char| {
                matches!(c, ',' | '.' | ':' | ';' | ')' | '(' | '"' | '\'' | '`')
            });
            if !candidate.contains('/') {
                return None;
            }
            let ext = std::path::Path::new(candidate)
                .extension()
                .and_then(|e| e.to_str())?;
            if interesting_extensions
                .iter()
                .any(|ie| ext.eq_ignore_ascii_case(ie))
            {
                Some(candidate.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Truncate content to a character budget, appending '…' if truncated.
fn truncate_summary(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let mut truncated: String = content.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
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

    #[test]
    fn micro_compact_clears_tool_role_messages() {
        let config = CompactorConfig {
            preservation_turns: 1,
            ..Default::default()
        };
        let compactor = Compactor::new(config);
        let messages = vec![
            ChatMessage::tool_result("tc-1", "Big tool output that bloats context..."),
            ChatMessage::user("latest question"),
        ];
        let result = compactor.micro_compact(&messages).unwrap();
        assert_eq!(result[0].content, CLEARED_MARKER);
        assert_eq!(result[1].content, "latest question");
    }

    #[test]
    fn micro_compact_preserves_recent_tool_results() {
        let config = CompactorConfig {
            preservation_turns: 2,
            ..Default::default()
        };
        let compactor = Compactor::new(config);
        let messages = vec![
            ChatMessage::tool_result("tc-old", "old result"),
            ChatMessage::tool_result("tc-recent", "recent result"),
        ];
        // Both messages are within preservation window (2 turns)
        assert!(compactor.micro_compact(&messages).is_none());
    }

    #[test]
    fn micro_compact_skips_already_cleared() {
        let config = CompactorConfig {
            preservation_turns: 1,
            ..Default::default()
        };
        let compactor = Compactor::new(config);
        let messages = vec![
            ChatMessage::tool_result("tc-1", CLEARED_MARKER),
            ChatMessage::user("latest question"),
        ];
        // Already cleared — no modification needed
        assert!(compactor.micro_compact(&messages).is_none());
    }

    #[test]
    fn structured_local_compact_produces_summary() {
        let config = CompactorConfig {
            preservation_turns: 1,
            max_history_tokens: 10,
            ..Default::default()
        };
        let compactor = Compactor::new(config);
        let messages = vec![
            ChatMessage::user("Investigate src/main.rs"),
            ChatMessage::assistant("I'll look at src/main.rs now."),
            ChatMessage::user("Also fix crates/core/lib.rs"),
            ChatMessage::assistant("Working on it."),
            ChatMessage::user("What's the status?"),
        ];
        let result = compactor.structured_local_compact(&messages);
        // Should preserve the last message and compact the rest
        assert!(result.len() <= 3);
        // First message should be the continuation
        assert!(result[0].content.contains("session is being continued"));
        assert!(result[0].content.contains("summary"));
    }

    #[test]
    fn safe_split_point_respects_tool_pairs() {
        let config = CompactorConfig {
            preservation_turns: 1,
            ..Default::default()
        };
        let compactor = Compactor::new(config);
        let messages = vec![
            ChatMessage::user("Search for files"),
            ChatMessage::assistant_with_tool_calls(
                "I'll search.",
                vec![crate::ToolCallRequest {
                    id: "tc-1".into(),
                    name: "search".into(),
                    arguments: "{}".into(),
                }],
            ),
            ChatMessage::tool_result("tc-1", "found 5 files"),
            ChatMessage::assistant("Here are the results."),
        ];
        let split = compactor.find_safe_split_point(&messages);
        // Should NOT split between the assistant(tool_calls) and tool_result
        // The split should be at or before index 1
        assert!(
            split <= 1 || split >= 3,
            "split={split} would orphan a tool result"
        );
    }

    #[test]
    fn extract_file_candidates_finds_paths() {
        let text = "Update src/main.rs and fix crates/core/lib.rs next.";
        let files = extract_file_candidates(text);
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"crates/core/lib.rs".to_string()));
    }

    #[test]
    fn truncate_summary_truncates_long_text() {
        let long = "x".repeat(300);
        let result = truncate_summary(&long, 100);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 101);
    }
}
