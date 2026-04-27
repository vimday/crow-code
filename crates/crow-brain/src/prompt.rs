pub struct PromptBuilder {
    components: Vec<String>,
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptBuilder {
    pub fn new() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    pub fn with_system_instruction(mut self, instruction: &str) -> Self {
        self.components.push(instruction.to_string());
        self
    }

    pub fn with_schema_guide(mut self, schema: &str) -> Self {
        self.components.push(format!("Output ONLY valid JSON matching the AgentAction schema.\n\n{schema}"));
        self
    }

    pub fn with_context(mut self, context: &str) -> Self {
        self.components.push(format!("[CONTEXT]\n{context}"));
        self
    }

    pub fn with_error_feedback(mut self, error: &str) -> Self {
        self.components.push(format!("[SYSTEM: PREVIOUS ATTEMPT FAILED]\nYour previous JSON output was invalid.\nError:\n{error}\n\nPlease fix the JSON to strictly conform to the schema."));
        self
    }

    pub fn with_validation_feedback(mut self, reason: &str) -> Self {
        self.components.push(format!("[SYSTEM: PREVIOUS ATTEMPT FAILED]\nYour JSON was syntactically valid but semantically invalid.\nReason: {reason}\n\nPlease fix and resubmit."));
        self
    }

    pub fn with_verifier_feedback(mut self, outcome: &str, log: &str) -> Self {
        self.components.push(format!("[VERIFICATION FAILED]\nYour previous plan resulted in a failed test execution.\nOutcome: {outcome}\nLog:\n{log}\n\nPlease reflect and output a new AgentAction to fix the issue. If you need to read more files to understand the failure, use the read_files action."));
        self
    }

    pub fn build(self) -> String {
        self.components.join("\n\n")
    }
}

pub struct CompactionPrompt {
    prompt: String,
}

impl CompactionPrompt {
    pub fn new(base_prompt: &str) -> Self {
        Self {
            prompt: format!(
                "[SYSTEM COMPACTION REQUEST]\n\
                {base_prompt}\n\
                \n\
                Return ONLY the summary wrapped in `<summary>...</summary>` tags, without any other text. Do NOT emit a JSON AgentAction."
            ),
        }
    }

    pub fn build(self) -> String {
        self.prompt
    }
}
