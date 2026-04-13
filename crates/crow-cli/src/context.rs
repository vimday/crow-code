use crow_brain::{ChatMessage, ChatRole};
use std::collections::VecDeque;

/// Manages the LLM context envelope.
/// Ensures that the cognitive loop does not blow out the token window
/// with massive file reads or lengthy verification logs over multiple retries.
pub struct CognitiveBudget {
    system_messages: Vec<ChatMessage>,
    conversation: VecDeque<ChatMessage>,
    max_bytes: usize,
    max_history_turns: usize,
}

impl CognitiveBudget {
    pub fn new(sys_msg: ChatMessage) -> Self {
        Self {
            system_messages: vec![sys_msg],
            conversation: VecDeque::new(),
            // 768 KB default hard cap on context size.
            max_bytes: 768 * 1024,
            max_history_turns: 30, // 30 messages max
        }
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.conversation.push_back(ChatMessage::user(content));
        self.enforce_budget();
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.conversation.push_back(ChatMessage::assistant(content));
        self.enforce_budget();
    }

    /// Adds the result of a file read to the context.
    /// Truncates the text if the file is massive.
    pub fn push_file_read(&mut self, content: String) {
        let max_read = 150 * 1024; // Limit single read to 150KB
        if content.len() > max_read {
            let mut safe_len = max_read;
            while safe_len > 0 && !content.is_char_boundary(safe_len) {
                safe_len -= 1;
            }
            let truncated = format!(
                "{}...\n\n[SYSTEM: File content truncated at 150KB to preserve context budget]",
                &content[..safe_len]
            );
            self.push_user(truncated);
        } else {
            self.push_user(content);
        }
    }

    /// Enforces size limits by aggressively pruning older messages.
    fn enforce_budget(&mut self) {
        // Strip out the oldest messages if we exceed the turn count.
        while self.conversation.len() > self.max_history_turns {
            self.conversation.pop_front();
        }

        // If we exceed byte budget, replace the oldest user message with a pruning marker
        // until we fit or run out of old messages.
        while self.total_bytes() > self.max_bytes && self.conversation.len() > 2 {
            let removed = self.conversation.pop_front();
            if let Some(msg) = removed {
                if msg.role == ChatRole::User
                    && !msg.content.starts_with("[SYSTEM: Older context pruned")
                {
                    self.conversation.push_front(ChatMessage::user(
                        "[SYSTEM: Older context pruned to save budget]",
                    ));
                    // Check if it fits now, else pop again loop
                }
            }
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
                .map(|s| s.content.len())
                .sum::<usize>()
    }

    /// Export the bounded context window for the LLM client.
    pub fn as_messages(&self) -> Vec<ChatMessage> {
        let mut out = self.system_messages.clone();
        out.extend(self.conversation.iter().cloned());
        out
    }
}
