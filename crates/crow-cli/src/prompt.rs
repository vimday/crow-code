use crow_intel::ContextMap;
use crow_patch::SnapshotId;

pub struct PromptBuilder {
    identity: String,
    project_context: String,
    developer_instructions: String,
    skills: String,
    context_map: String,
    contract: String,
    platform_context: String,
    compaction_prompt: Option<String>,
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
            developer_instructions: String::new(),
            skills: String::new(),
            context_map: String::new(),
            contract: String::new(),
            platform_context: build_platform_context(),
            compaction_prompt: None,
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

    /// Codex-style developer instructions (from AGENTS.md or config).
    /// These are project-level rules that supplement the base instructions.
    pub fn with_developer_instructions(mut self, instructions: &str) -> Self {
        self.developer_instructions = instructions.to_string();
        self
    }

    pub fn with_context_map(mut self, context_map: &ContextMap, snapshot_id: &SnapshotId) -> Self {
        self.context_map = format!(
            "Context (AST Map):\n{}\n\nWorkspace Snapshot ID: {}",
            context_map.map_text, snapshot_id.0
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

    /// Set a custom compaction prompt for context checkpoint compaction.
    /// If not set, the default compaction prompt is used.
    pub fn with_compaction_prompt(mut self, prompt: &str) -> Self {
        self.compaction_prompt = Some(prompt.to_string());
        self
    }

    /// Returns the compaction prompt for this session (Codex-style handoff summary).
    pub fn compaction_prompt(&self) -> &str {
        self.compaction_prompt
            .as_deref()
            .unwrap_or(DEFAULT_COMPACTION_PROMPT)
    }

    pub fn build(self) -> Vec<crow_brain::ChatMessage> {
        let mut sys_prompt = String::new();

        // Layer 1: Platform context (Codex injects current_date, timezone, OS)
        if !self.platform_context.is_empty() {
            sys_prompt.push_str(&self.platform_context);
            sys_prompt.push_str("\n\n");
        }

        // Layer 2: Project context (persistent memory, workspace rules)
        if !self.project_context.is_empty() {
            sys_prompt.push_str(&self.project_context);
            sys_prompt.push_str("\n\n");
        }

        // Layer 3: Developer instructions (AGENTS.md / config)
        if !self.developer_instructions.is_empty() {
            sys_prompt.push_str("--- developer instructions ---\n\n");
            sys_prompt.push_str(&self.developer_instructions);
            sys_prompt.push_str("\n\n");
        }

        // Layer 4: Context map (AST/repo structure)
        sys_prompt.push_str(&self.context_map);
        sys_prompt.push_str("\n\n");

        // Layer 5: Skills and MCP tools
        if !self.skills.is_empty() {
            sys_prompt.push_str(&self.skills);
            sys_prompt.push_str("\n\n");
        }

        // Layer 6: Contract (constraints, snapshot ID)
        sys_prompt.push_str(&self.contract);

        vec![
            crow_brain::ChatMessage::system(self.identity),
            crow_brain::ChatMessage::system(sys_prompt),
        ]
    }
}

/// Build platform context (Codex injects current_date, timezone, cwd, OS).
fn build_platform_context() -> String {
    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d").to_string();
    let time = now.format("%H:%M:%S").to_string();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());

    format!(
        "## Platform Context\n\
         - Current date: {date}\n\
         - Current time: {time}\n\
         - Operating system: {os} ({arch})\n\
         - Working directory: {cwd}\n\
         - Default shell: {shell}"
    )
}

/// Codex-style compaction prompt. Creates a handoff summary for context checkpoint.
pub const DEFAULT_COMPACTION_PROMPT: &str = r"You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.

Include:
- Current progress and key decisions made
- Important context, constraints, or user preferences
- What remains to be done (clear next steps)
- Any critical data, examples, or references needed to continue

Be concise, structured, and focused on helping the next LLM seamlessly continue the work.";

/// Rich, behaviorally-tuned identity prompt inspired by Codex's base_instructions.
/// Clear sections for identity, task execution, tool use, and tone.
const DEFAULT_IDENTITY: &str = r"You are Crow, an autonomous evidence-driven coding agent.

# System
- You are an expert software engineer working autonomously.
- You communicate with the user through your plan rationale. Keep responses concise and technical.
- You have access to tools for reading files, searching code, listing directories, and executing bounded commands.
- When presented with an ambiguous task, proactively gather context before making changes.

# Doing Tasks
- The user will primarily request software engineering tasks: solving bugs, adding functionality, refactoring, explaining code, etc.
- ALWAYS read relevant files before modifying them. Understand existing code patterns before suggesting modifications.
- Do not create files unless absolutely necessary. Prefer editing existing files.
- If an approach fails, diagnose why before switching tactics — read the error, check assumptions, try a focused fix.
- Be careful not to introduce security vulnerabilities. Prioritize writing safe, correct code.
- Write clean, idiomatic code that follows the style of the existing codebase.
- When you encounter test failures, investigate the root cause rather than blindly modifying tests.

# Tool Use
- Use tools efficiently. Batch reads when possible.
- Carefully consider reversibility and blast radius before each action:
  - Read operations (file reads, searches, directory listing): proceed freely
  - Code modifications: apply through the structured IntentPlan system with precise hunks
  - Never modify files outside the workspace root
- When tool output is large, extract only the relevant parts for your analysis.
- If a tool call fails, read the error message carefully before retrying.

# Tone and Style
- Your responses should be short, technical, and precise.
- When explaining code, use concrete references to file paths and line numbers.
- For conversational responses (no code changes needed), submit a plan with an empty operations array.
- Avoid unnecessary preamble. Get to the point.
";
