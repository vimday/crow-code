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
    /// Optional role overlay prompt (e.g., AgentRole.system_prompt_suffix).
    /// Merged at the end of the base prompt to inject role-specific behavior.
    role_overlay: Option<String>,
    /// Tool usage instructions section.
    /// Provides structured guidance on how to use each registered tool.
    tool_instructions: Vec<String>,
    /// Structured environment context (Codex ContextualUserFragment pattern).
    /// Renders as `<environment_context>` XML block.
    environment_context: Option<crate::environment::CrowEnvironmentContext>,
    /// Permission-aware instructions (Codex permissions_instructions pattern).
    /// Renders as `<permissions_instructions>` XML block.
    permissions_prompt: Option<crate::permissions_prompt::PermissionsPrompt>,
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

    /// Merge an agent role overlay into the system prompt.
    /// This is used to inject role-specific behavioral instructions
    /// (e.g., explorer = read-only, reviewer = code review focus).
    #[must_use]
    pub fn with_role_overlay(mut self, overlay: impl Into<String>) -> Self {
        self.role_overlay = Some(overlay.into());
        self
    }

    /// Add tool usage instructions from tool definitions.
    /// Generates structured guidance for each tool (Codex model_instructions pattern).
    /// Attach structured environment context (Codex `ContextualUserFragment` pattern).
    /// Renders as `<environment_context>` XML block in the system prompt.
    #[must_use]
    pub fn with_environment(mut self, env: crate::environment::CrowEnvironmentContext) -> Self {
        self.environment_context = Some(env);
        self
    }

    /// Attach permission-aware instructions (Codex `permissions_instructions` pattern).
    /// Tells the agent exactly what it can and cannot do.
    #[must_use]
    pub fn with_permissions(mut self, perms: crate::permissions_prompt::PermissionsPrompt) -> Self {
        self.permissions_prompt = Some(perms);
        self
    }

    #[must_use]
    pub fn with_tool_instructions(mut self, tool_defs: &[serde_json::Value]) -> Self {
        for def in tool_defs {
            let name = def["function"]["name"].as_str().unwrap_or("unknown");
            let desc = def["function"]["description"].as_str().unwrap_or("");
            self.tool_instructions.push(format!("- **{name}**: {desc}"));
        }
        self
    }

    /// Build the final system prompt string.
    pub fn build(self) -> String {
        let base = self
            .base_prompt
            .unwrap_or("You are Crow, a helpful AI coding assistant.")
            .trim();

        let mut prompt = base.to_string();

        // Append role overlay (Codex AgentRole pattern)
        if let Some(overlay) = &self.role_overlay {
            if !overlay.is_empty() {
                prompt.push_str("\n\n# Role\n");
                prompt.push_str(overlay);
            }
        }

        // Append structured environment context (Codex ContextualUserFragment)
        if let Some(env) = &self.environment_context {
            prompt.push_str("\n\n");
            prompt.push_str(&env.render());
        }

        // Append permission instructions (Codex permissions_instructions)
        if let Some(perms) = &self.permissions_prompt {
            prompt.push_str("\n\n");
            prompt.push_str(&perms.render());
        }

        // Append project memory sections
        for section in &self.memory_sections {
            prompt.push_str("\n\n");
            prompt.push_str(section);
        }

        // Append tool instructions section (Codex model_instructions pattern)
        if !self.tool_instructions.is_empty() {
            prompt.push_str("\n\n# Available Tools\n\n");
            prompt.push_str("You have access to the following tools:\n");
            for instruction in &self.tool_instructions {
                prompt.push_str(instruction);
                prompt.push('\n');
            }
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
