use crow_intel::RepoMap;
use crow_patch::SnapshotId;

pub struct PromptBuilder {
    identity: String,
    project_context: String,
    skills: String,
    repo_map: String,
    contract: String,
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptBuilder {
    pub fn new() -> Self {
        Self {
            identity: DEFAULT_IDENTITY.to_string(),
            project_context: String::new(),
            skills: String::new(),
            repo_map: String::new(),
            contract: String::new(),
        }
    }

    pub fn with_identity(mut self, identity: &str) -> Self {
        self.identity = identity.to_string();
        self
    }

    pub fn with_project_context(mut self, context: &str) -> Self {
        self.project_context = context.to_string();
        self
    }

    pub fn with_repo_map(mut self, repo_map: &RepoMap, snapshot_id: &SnapshotId) -> Self {
        self.repo_map = format!(
            "Context (Repository Map):\n{}\n\nWorkspace Snapshot ID: {}",
            repo_map.map_text, snapshot_id.0
        );
        self
    }

    pub fn with_mcp(mut self, mcp_manager: Option<&crow_runtime::mcp::McpManager>) -> Self {
        if let Some(mgr) = mcp_manager {
            let mcp_ctx = mgr.prompt_context();
            if !mcp_ctx.is_empty() {
                self.skills.push_str("MCP Interface:\n");
                self.skills.push_str(mcp_ctx);
            }
        }
        self
    }

    pub fn with_dynamic_skills(mut self, skills: &[crow_brain::skill::Skill]) -> Self {
        if !skills.is_empty() {
            self.skills
                .push_str("\n\n## Available Skills\n\nLoad the following skills on demand\n");
            for skill in skills {
                let location = skill.source_path.to_string_lossy();
                let triggers = skill.triggers.join(", ");
                self.skills.push_str(&format!(
                    "<skill name=\"{}\" location=\"{}\" triggers=\"{}\">{}</skill>\n",
                    skill.name, location, triggers, skill.description
                ));
            }
        }
        self
    }

    pub fn with_contract(mut self, snapshot_id: &SnapshotId) -> Self {
        self.contract = format!(
            "IMPORTANT: When you submit a plan, set base_snapshot_id to \"{}\" exactly.\n\n\
            Constraints: Please limit your edits to Create and Modify operations if possible.\n\n\
            MCTS DYNAMIC SEARCH: For complex code refactors, we use rigorous parallel searches (MCTS). \
            However, if your intended changes are TRIVIAL (e.g. pure documentation tweaks, simple text formatting, \
            or modifying markdown files), please explicitly set `requires_mcts = false` to save API latency.",
            snapshot_id.0
        );
        self
    }

    pub fn build(self) -> Vec<crow_brain::ChatMessage> {
        let mut sys_prompt = String::new();

        if !self.project_context.is_empty() {
            sys_prompt.push_str(&self.project_context);
            sys_prompt.push_str("\n\n");
        }

        sys_prompt.push_str(&self.repo_map);
        sys_prompt.push_str("\n\n");

        if !self.skills.is_empty() {
            sys_prompt.push_str(&self.skills);
            sys_prompt.push_str("\n\n");
        }

        sys_prompt.push_str(&self.contract);

        vec![
            crow_brain::ChatMessage::system(self.identity),
            crow_brain::ChatMessage::system(sys_prompt),
        ]
    }
}

/// Rich, behaviorally-tuned identity prompt inspired by yomi's architecture.
/// Clear sections for identity, task execution, safety, and tone.
const DEFAULT_IDENTITY: &str = r"You are Crow, an autonomous evidence-driven coding agent.

# System
- You communicate with the user through your plan rationale. Keep responses concise and technical.
- You have access to tools for reading files, searching code, listing directories, and executing bounded commands.

# Doing Tasks
- The user will primarily request software engineering tasks: solving bugs, adding functionality, refactoring, explaining code, etc.
- ALWAYS read files before modifying them. Understand existing code before suggesting modifications.
- Do not create files unless absolutely necessary. Prefer editing existing files.
- If an approach fails, diagnose why before switching tactics — read the error, check assumptions, try a focused fix.
- Be careful not to introduce security vulnerabilities. Prioritize writing safe, correct code.

# Executing Actions
Carefully consider reversibility and blast radius:
- Read operations (file reads, searches, directory listing): proceed freely
- Code modifications: apply through the structured IntentPlan system with precise hunks
- Never modify files outside the workspace root

# Tone and Style
- Your responses should be short, technical, and precise.
- When explaining code, use concrete references to file paths and line numbers.
- For conversational responses (no code changes needed), submit a plan with an empty operations array.
";
