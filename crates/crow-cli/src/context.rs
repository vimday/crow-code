use crow_brain::{ChatMessage, ChatRole};
use std::collections::VecDeque;

#[derive(Debug, Clone)]
struct Memory {
    pub message: ChatMessage,
    /// What this message becomes when it is compressed due to budget constraints.
    pub summary: Option<String>,
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

impl ConversationManager {
    pub fn new(sys_msgs: Vec<ChatMessage>) -> Self {
        Self {
            system_messages: sys_msgs,
            conversation: VecDeque::new(),
            // 768 KB default hard cap on context size.
            max_bytes: 768 * 1024,
            max_history_turns: 30, // 30 messages max
        }
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
            let mut safe_len = max_read;
            while safe_len > 0 && !content.is_char_boundary(safe_len) {
                safe_len -= 1;
            }
            format!(
                "{}...\n\n[SYSTEM: File content truncated at 150KB to preserve context budget]",
                &content[..safe_len]
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
        let truncated_error = &first_error[..first_error.len().min(200)];

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
        // 1. Hard bound on absolute history length (prevents infinite loop growth)
        while self.conversation.len() > self.max_history_turns {
            self.conversation.pop_front();
        }

        // 2. Semantic Pruning: If we exceed byte budget, downgrade oldest un-summarized
        //    user messages to their summaries.
        let mut idx = 0;
        while self.total_bytes() > self.max_bytes && idx < self.conversation.len() {
            let mem = &mut self.conversation[idx];
            // If it has a summary and it's not ALREADY a summary
            // (we distinguish by just replacing the message and removing the summary Option)
            if let Some(summary) = mem.summary.take() {
                mem.message = ChatMessage::user(summary);
            } else if mem.message.role == ChatRole::Assistant {
                // If it's an assistant message without a summary, we can just clear it to save space
                mem.message.content.clear();
            }
            idx += 1;
        }

        // 3. Last Resort: If even after collapsing all summaries we are still over budget, hard pop
        while self.total_bytes() > self.max_bytes && self.conversation.len() > 2 {
            self.conversation.pop_front();
        }
    }

    fn total_bytes(&self) -> usize {
        self.system_messages
            .iter()
            .map(|s| s.content.len())
            .sum::<usize>()
            + self
                .conversation
                .iter()
                .map(|m| m.message.content.len())
                .sum::<usize>()
    }

    /// Export the bounded context window for the LLM client.
    pub fn as_messages(&self) -> Vec<ChatMessage> {
        let mut out = self.system_messages.clone();
        out.extend(
            self.conversation
                .iter()
                .filter(|m| !m.message.content.is_empty()) // Drop empty assistant stubs
                .map(|m| m.message.clone()),
        );
        out
    }
}
