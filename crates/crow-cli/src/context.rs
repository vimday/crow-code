use crow_brain::{ChatMessage, ChatRole};
use std::collections::VecDeque;

#[derive(Debug, Clone)]
struct Memory {
    message: ChatMessage,
    /// What this message becomes when it is compressed due to budget constraints.
    summary: Option<String>,
}

/// Manages the LLM context envelope semantic strategies.
/// Ensures that the cognitive loop does not blow out the token window
/// with massive file reads or lengthy verification logs over multiple retries.
pub struct ConversationManager {
    system_messages: Vec<ChatMessage>,
    conversation: VecDeque<Memory>,
    max_bytes: usize,
    max_history_turns: usize,
}

fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    crow_patch::safe_truncate(s, max_bytes)
}

impl ConversationManager {
    pub fn new(mut sys_msgs: Vec<ChatMessage>) -> Self {
        use crate::budget::{MAX_CONTEXT_BYTES, MAX_HISTORY_TURNS, MAX_SYSTEM_BYTES};

        let sys_bytes: usize = sys_msgs.iter().map(|s| s.content.len()).sum();
        if sys_bytes > MAX_SYSTEM_BYTES {
            // If system context is too large, truncate the largest message (typically the repo map)
            if let Some(largest) = sys_msgs.iter_mut().max_by_key(|s| s.content.len()) {
                let orig_len = largest.content.len();
                // Because there are multiple system messages, we give the largest one whatever
                // space is left after accounting for the other system messages.
                let other_bytes = sys_bytes - orig_len;

                // Pre-compute the suffix so we can subtract its length from the content budget.
                // This prevents the formatted result from overshooting MAX_SYSTEM_BYTES.
                let suffix = format!(
                    "...\n\n[SYSTEM: Anchor context truncated (original size {} bytes) to preserve conversation budget]",
                    orig_len
                );
                let content_budget = MAX_SYSTEM_BYTES
                    .saturating_sub(other_bytes)
                    .saturating_sub(suffix.len());

                let truncated = safe_truncate(&largest.content, content_budget);
                largest.content = format!("{}{}", truncated, suffix);
            }
        }

        Self {
            system_messages: sys_msgs,
            conversation: VecDeque::new(),
            max_bytes: MAX_CONTEXT_BYTES,
            max_history_turns: MAX_HISTORY_TURNS,
        }
    }

    pub fn set_system(&mut self, sys_msgs: Vec<ChatMessage>) {
        self.system_messages = sys_msgs;
        self.enforce_budget();
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.conversation.push_back(Memory {
            message: ChatMessage::user(content),
            summary: Some("[SYSTEM: Older user context pruned to save budget]".into()),
        });
        self.enforce_budget();
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.conversation.push_back(Memory {
            message: ChatMessage::assistant(content),
            summary: None, // Assistant messages are completely pruned if necessary
        });
        self.enforce_budget();
    }

    /// Adds the result of a file read to the context.
    /// Truncates the text if the file is massive, and provides a semantic summary for pruning.
    pub fn push_file_read(&mut self, paths: &[String], content: String) {
        let max_read = 150 * 1024; // Limit single read to 150KB
        let final_content = if content.len() > max_read {
            format!(
                "{}...\n\n[SYSTEM: File content truncated at 150KB to preserve context budget]",
                safe_truncate(&content, max_read)
            )
        } else {
            content
        };

        // When this massive file dump ages out, we retain the memory of WHAT was read
        let summary = format!(
            "[SYSTEM: Agent previously read files: {:?}. Full content pruned from history.]",
            paths
        );

        self.conversation.push_back(Memory {
            message: ChatMessage::user(final_content),
            summary: Some(summary),
        });
        self.enforce_budget();
    }

    /// Appends a verification result to the conversation.
    /// When pruned, the huge log is dropped but the logical outcome is preserved in the summary.
    pub fn push_verifier_result(&mut self, outcome_str: &str, log: &str) {
        let content = format!(
            "[VERIFICATION FAILED]\nYour previous plan resulted in a failed test execution.\nOutcome: {}\nLog:\n{}\n\nPlease reflect and output a new AgentAction to fix the issue. If you need to read more files to understand the failure, use the read_files action.",
            outcome_str, log
        );

        // Extract first error-like line for a richer pruned summary
        let first_error = log
            .lines()
            .find(|l| {
                let lower = l.to_lowercase();
                lower.contains("error") || lower.contains("failed") || lower.contains("panicked")
            })
            .unwrap_or("(no error line extracted)");
        let truncated_error = safe_truncate(first_error, 200);

        let summary = format!(
            "[SYSTEM: Previous verification failed ({}). First error: {}. Full logs pruned.]",
            outcome_str, truncated_error
        );

        self.conversation.push_back(Memory {
            message: ChatMessage::user(content),
            summary: Some(summary),
        });
        self.enforce_budget();
    }

    /// Appends a reconnaissance result to the conversation with domain-aware compression.
    /// When the full output ages out, a compact semantic summary is preserved.
    pub fn push_recon_result(&mut self, tool_name: &str, description: &str, output: &str) {
        let content = format!(
            "[RECON RESULT]\nTool: {}\nCommand: {}\nOutput:\n{}",
            tool_name, description, output
        );

        // Generate a domain-aware summary based on the tool type
        let line_count = output.lines().count();
        let summary = match tool_name {
            "list_dir" => format!(
                "[SYSTEM: Listed {} entries via `{}`. Full output pruned.]",
                line_count, description
            ),
            "search" => {
                let match_count = output
                    .lines()
                    .filter(|l| !l.is_empty() && !l.starts_with("--"))
                    .count();
                format!(
                    "[SYSTEM: Search found ~{} matches via `{}`. Full output pruned.]",
                    match_count, description
                )
            }
            "dir_tree" => format!(
                "[SYSTEM: Tree showed {} lines via `{}`. Full output pruned.]",
                line_count, description
            ),
            _ => format!(
                "[SYSTEM: Recon `{}` returned {} lines. Full output pruned.]",
                description, line_count
            ),
        };

        self.conversation.push_back(Memory {
            message: ChatMessage::user(content),
            summary: Some(summary),
        });
        self.enforce_budget();
    }

    /// Enforces size limits by aggressively pruning older messages.
    fn enforce_budget(&mut self) {
        // 1. Semantic Pruning: If we exceed byte budget, downgrade oldest un-summarized
        //    user messages to their summaries. We skip index 0 to anchor the original Task.
        let mut idx = 1;
        while self.check_over_budget() && idx < self.conversation.len() {
            let mem = &mut self.conversation[idx];
            // If it has a summary and it's not ALREADY a summary
            // (we distinguish by just replacing the message and removing the summary Option)
            if let Some(summary) = mem.summary.take() {
                mem.message = ChatMessage::user(summary);
            } else if mem.message.role == ChatRole::Assistant {
                // Replace with a minimal placeholder to maintain strict
                // User→Assistant→User role alternation required by some
                // providers (e.g. Anthropic). Never clear() — that causes
                // the filter in as_messages() to drop the message entirely,
                // creating consecutive User messages and triggering HTTP 400.
                mem.message.content = "[pruned]".into();
            }
            idx += 1;
        }

        // 2. Hard bound on absolute history length (prevents infinite loop growth)
        // Keep index 0 alive, so we remove from index 1.
        while self.conversation.len() > self.max_history_turns && self.conversation.len() > 1 {
            self.conversation.remove(1);
        }

        // 3. Last Resort: If even after collapsing all summaries we are still over budget, hard pop
        // Keep index 0 alive, so we remove from index 1.
        while self.check_over_budget() && self.conversation.len() > 1 {
            self.conversation.remove(1);
        }
    }

    fn history_bytes(&self) -> usize {
        self.conversation
            .iter()
            .map(|m| m.message.content.len())
            .sum::<usize>()
    }

    fn system_bytes(&self) -> usize {
        self.system_messages
            .iter()
            .map(|s| s.content.len())
            .sum::<usize>()
    }

    fn check_over_budget(&self) -> bool {
        // We are over budget if total bytes exceeds max_bytes AND the history itself
        // is taking up a meaningful amount of space (> MIN_HISTORY_RESERVE).
        // This prevents an oversized system message from permanently clamping history
        // to exactly 1 message and effectively lobotomizing the agent.
        let total = self.system_bytes() + self.history_bytes();
        total > self.max_bytes && self.history_bytes() > crate::budget::MIN_HISTORY_RESERVE
    }

    /// Export the bounded context window for the LLM client.
    ///
    /// All messages are emitted — even pruned ones with placeholder content —
    /// to preserve the strict role alternation that providers like Anthropic require.
    pub fn as_messages(&self) -> Vec<ChatMessage> {
        let mut out = self.system_messages.clone();
        out.extend(self.conversation.iter().map(|m| m.message.clone()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manager_truncates_oversized_system_context() {
        // Create an enormously large system message (approx 1 MB)
        let large_repo_map = "a".repeat(1024 * 1024);
        let sys_msgs = vec![
            ChatMessage::system("You are an agent"),
            ChatMessage::system(&large_repo_map),
        ];

        let manager = ConversationManager::new(sys_msgs);

        // Max sys bytes is 768KB - 64KB = 704KB.
        // The suffix is now pre-budgeted, so the total must be strictly within bounds.
        let total_sys_len: usize = manager
            .system_messages
            .iter()
            .map(|m| m.content.len())
            .sum();

        assert!(
            total_sys_len <= crate::budget::MAX_SYSTEM_BYTES,
            "System messages must fit within budget. Found: {} bytes, limit: {} bytes",
            total_sys_len,
            crate::budget::MAX_SYSTEM_BYTES
        );

        let anchor = &manager.system_messages[1].content;
        assert!(anchor.contains("[SYSTEM: Anchor context truncated"));
    }

    #[test]
    fn enforce_budget_preserves_anchor_when_budget_maxed() {
        let sys_msgs = vec![ChatMessage::system("Sys")];
        let mut manager = ConversationManager::new(sys_msgs);

        // Add task anchor (index 0)
        manager.push_user("TASK: Write a web server");

        // Force a bloat in history
        let blob = "b".repeat(80 * 1024);
        for i in 0..15 {
            manager.push_assistant(format!("Response iteration {}", i));
            manager.push_file_read(&[format!("file_{}.rs", i)], blob.clone());
        }

        manager.enforce_budget();

        // History shouldn't be completely wiped. Should retain the task anchor (index 0).
        let conv = &manager.conversation;
        assert_eq!(conv[0].message.content, "TASK: Write a web server");
        assert!(
            conv.len() > 1,
            "Should keep at least some condensed history"
        );
    }
}
