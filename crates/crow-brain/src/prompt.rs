use std::fmt::Write;

use crate::skill::Skill;

/// Builder for constructing the epistemic loop prompt with
/// structured component composition.
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
        self.components.push(format!(
            "Output ONLY valid JSON matching the AgentAction schema.\n\n{schema}"
        ));
        self
    }

    pub fn with_context(mut self, context: &str) -> Self {
        self.components.push(format!("[CONTEXT]\n{context}"));
        self
    }

    pub fn with_error_feedback(mut self, error: &str) -> Self {
        self.components.push(format!(
            "[SYSTEM: PREVIOUS ATTEMPT FAILED]\n\
             Your previous JSON output was invalid.\n\
             Error:\n{error}\n\n\
             Please fix the JSON to strictly conform to the schema."
        ));
        self
    }

    pub fn with_validation_feedback(mut self, reason: &str) -> Self {
        self.components.push(format!(
            "[SYSTEM: PREVIOUS ATTEMPT FAILED]\n\
             Your JSON was syntactically valid but semantically invalid.\n\
             Reason: {reason}\n\n\
             Please fix and resubmit."
        ));
        self
    }

    pub fn with_verifier_feedback(mut self, outcome: &str, log: &str) -> Self {
        self.components.push(format!(
            "[VERIFICATION FAILED]\n\
             Your previous plan resulted in a failed test execution.\n\
             Outcome: {outcome}\n\
             Log:\n{log}\n\n\
             Please reflect and output a new AgentAction to fix the issue. \
             If you need to read more files to understand the failure, use the read_files action."
        ));
        self
    }

    pub fn build(self) -> String {
        self.components.join("\n\n")
    }
}

/// Builder for system prompts with skill integration (yomi pattern).
///
/// Composes the base system prompt with available skills metadata,
/// instructing the agent to scan and load relevant skills on demand.
/// This replaces ad-hoc skill injection with a standardized pipeline.
#[derive(Debug, Default)]
pub struct SystemPromptBuilder<'a> {
    base_prompt: Option<&'a str>,
    skills: &'a [Skill],
    memory_sections: Vec<String>,
}

const SKILL_SECTION_HEADER: &str = "\n\n# Skills\n\
    IMPORTANT: before replying, you must scan available skills and \
    load skill when task hits its description.\n\n";

impl<'a> SystemPromptBuilder<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the base system prompt text.
    #[must_use]
    pub const fn base_prompt(mut self, prompt: &'a str) -> Self {
        self.base_prompt = Some(prompt);
        self
    }

    /// Attach skills metadata to inject into the system prompt.
    #[must_use]
    pub const fn with_skills(mut self, skills: &'a [Skill]) -> Self {
        self.skills = skills;
        self
    }

    /// Add a project memory section (e.g. from AGENTS.md).
    #[must_use]
    pub fn with_memory(mut self, memory: impl Into<String>) -> Self {
        self.memory_sections.push(memory.into());
        self
    }

    /// Build the final system prompt string.
    pub fn build(self) -> String {
        let base = self
            .base_prompt
            .unwrap_or("You are Crow, a helpful AI coding assistant.")
            .trim();

        let mut prompt = base.to_string();

        // Append project memory sections
        for section in &self.memory_sections {
            prompt.push_str("\n\n");
            prompt.push_str(section);
        }

        // Append skills section
        if !self.skills.is_empty() {
            prompt.push_str(SKILL_SECTION_HEADER);
            prompt.push_str("## Available Skills\n");
            for skill in self.skills {
                let _ = write!(
                    prompt,
                    "name: {}\ndescription: {}\npath: {}\n\n",
                    skill.name,
                    skill.description,
                    skill.source_path.display()
                );
            }
        }

        prompt
    }
}

/// Structured compaction prompt for context summarization.
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
                Return ONLY the summary wrapped in `<summary>...</summary>` tags, \
                without any other text. Do NOT emit a JSON AgentAction."
            ),
        }
    }

    pub fn build(self) -> String {
        self.prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_system_prompt_builder_basic() {
        let prompt = SystemPromptBuilder::new()
            .base_prompt("You are Crow.")
            .build();
        assert_eq!(prompt, "You are Crow.");
    }

    #[test]
    fn test_system_prompt_builder_with_skills() {
        let skills = vec![Skill {
            name: "rust_debug".into(),
            description: "Debug Rust code".into(),
            triggers: vec!["rust".into()],
            env_dependencies: vec![],
            scope: crate::skill::SkillScope::Repo,
            source_path: PathBuf::from("/skills/rust_debug.md"),
        }];
        let prompt = SystemPromptBuilder::new()
            .base_prompt("Base")
            .with_skills(&skills)
            .build();
        assert!(prompt.contains("# Skills"));
        assert!(prompt.contains("rust_debug"));
        assert!(prompt.contains("Debug Rust code"));
    }

    #[test]
    fn test_system_prompt_builder_with_memory() {
        let prompt = SystemPromptBuilder::new()
            .base_prompt("Base")
            .with_memory("# Project Rules\nAlways use Result.")
            .build();
        assert!(prompt.contains("# Project Rules"));
        assert!(prompt.contains("Always use Result."));
    }

    #[test]
    fn test_prompt_builder_chain() {
        let prompt = PromptBuilder::new()
            .with_system_instruction("You are an agent.")
            .with_context("Working on project X.")
            .build();
        assert!(prompt.contains("You are an agent."));
        assert!(prompt.contains("[CONTEXT]"));
    }
}
