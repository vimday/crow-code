use crow_intel::ContextMap;
use crow_patch::SnapshotId;

pub struct PromptBuilder {
    identity: String,
    project_context: String,
    developer_instructions: String,
    git_context: String,
    skills: String,
    context_map: String,
    contract: String,
    platform_context: String,
    compaction_prompt: Option<String>,
    /// Tool usage guidance section (Codex model_instructions pattern).
    tool_instructions: String,
    /// Permission and policy context.
    permission_context: String,
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
            git_context: String::new(),
            skills: String::new(),
            context_map: String::new(),
            contract: String::new(),
            platform_context: build_platform_context(),
            compaction_prompt: None,
            tool_instructions: String::new(),
            permission_context: String::new(),
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

    /// Inject git context (branch, status, recent commits, diffs)
    /// into the system prompt. Provides the agent with situational
    /// awareness about the repository state (claw-code pattern).
    pub fn with_git_context(mut self, git_ctx: &crow_runtime::git_context::GitContext) -> Self {
        self.git_context = git_ctx.render();
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

    /// Inject tool usage instructions from registered tool definitions.
    /// Generates a structured guidance section (Codex model_instructions pattern).
    pub fn with_tool_instructions(mut self, tool_defs: &[serde_json::Value]) -> Self {
        if !tool_defs.is_empty() {
            self.tool_instructions.push_str("## Available Tools\n\n");
            self.tool_instructions
                .push_str("You have access to the following tools for this session:\n");
            for def in tool_defs {
                let name = def["function"]["name"].as_str().unwrap_or("unknown");
                let desc = def["function"]["description"].as_str().unwrap_or("");
                self.tool_instructions
                    .push_str(&format!("- **{name}**: {desc}\n"));
            }
        }
        self
    }

    /// Inject permission and policy context so the agent is aware of its boundaries.
    pub fn with_permission_context(
        mut self,
        mode: &str,
        workspace_root: &str,
    ) -> Self {
        self.permission_context = format!(
            "## Policy\n\
            - Permission mode: {mode}\n\
            - Workspace root: {workspace_root}\n\
            - You must not modify files outside the workspace root.\n\
            - Long threads and compactions may reduce accuracy. Keep interactions focused."
        );
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

        // Layer 2: Permission and policy context
        if !self.permission_context.is_empty() {
            sys_prompt.push_str(&self.permission_context);
            sys_prompt.push_str("\n\n");
        }

        // Layer 3: Project context (persistent memory, workspace rules)
        if !self.project_context.is_empty() {
            sys_prompt.push_str(&self.project_context);
            sys_prompt.push_str("\n\n");
        }

        // Layer 4: Developer instructions (AGENTS.md / config)
        if !self.developer_instructions.is_empty() {
            sys_prompt.push_str("--- developer instructions ---\n\n");
            sys_prompt.push_str(&self.developer_instructions);
            sys_prompt.push_str("\n\n");
        }

        // Layer 4.5: Git context (claw-code pattern)
        if !self.git_context.is_empty() {
            sys_prompt.push_str(&self.git_context);
            sys_prompt.push_str("\n\n");
        }

        // Layer 5: Tool usage instructions (Codex model_instructions pattern)
        if !self.tool_instructions.is_empty() {
            sys_prompt.push_str(&self.tool_instructions);
            sys_prompt.push_str("\n\n");
        }

        // Layer 6: Context map (AST/repo structure)
        sys_prompt.push_str(&self.context_map);
        sys_prompt.push_str("\n\n");

        // Layer 7: Skills and MCP tools
        if !self.skills.is_empty() {
            sys_prompt.push_str(&self.skills);
            sys_prompt.push_str("\n\n");
        }

        // Layer 8: Contract (constraints, snapshot ID)
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
pub const DEFAULT_COMPACTION_PROMPT: &str = r"You are performing a CONTEXT CHECKPOINT COMPACTION. Create a structured handoff summary for another LLM that will seamlessly resume the task.

Your summary MUST include these sections:

## 1. Outstanding User Requests
- What the user asked for (exact goal, not paraphrased)
- Current status: research / planning / implementing / verifying / complete

## 2. User Knowledge
- User preferences, constraints, or decisions they communicated
- Technology choices, coding style preferences, or explicit instructions

## 3. Key Decisions Made
- Architectural or design decisions and their rationale
- Alternatives considered and why they were rejected

## 4. Model Knowledge
- Important codebase discoveries (patterns, dependencies, gotchas)
- Error patterns encountered and solutions found
- Any failed approaches and why they failed

## 5. Modified Files
- List every file modified during this session with a brief description of changes
- Include line ranges if the changes were localized

## 6. Next Steps
- Concrete, actionable steps remaining to complete the task
- Priority order if there are multiple steps
- Any blockers or prerequisites

Be concise but complete. Prefer bullet points over prose. Include file paths and line numbers where relevant.
Do NOT include pleasantries, meta-commentary, or explanations of the compaction process itself.";

/// Rich, behaviorally-tuned identity prompt inspired by Codex's base_instructions.
/// Clear sections for identity, task execution, tool use, error recovery, and tone.
const DEFAULT_IDENTITY: &str = r"You are Crow, an autonomous evidence-driven coding agent.

# System
- You are an expert software engineer working autonomously.
- You communicate with the user through your plan rationale. Keep responses concise and technical.
- You have access to tools for reading files, searching code, listing directories, editing files, and executing bounded commands.
- When presented with an ambiguous task, proactively gather context before making changes.

# Doing Tasks
- The user will primarily request software engineering tasks: solving bugs, adding functionality, refactoring, explaining code, etc.
- ALWAYS read relevant files before modifying them. Understand existing code patterns and conventions first.
- Do not create files unless absolutely necessary. Prefer editing existing files.
- If an approach fails, diagnose why before switching tactics — read the error, check assumptions, try a focused fix.
- Be careful not to introduce security vulnerabilities. Prioritize writing safe, correct code.
- Write clean, idiomatic code that follows the style of the existing codebase.
- When you encounter test failures, investigate the root cause rather than blindly modifying tests.
- For complex multi-file changes, plan the order of edits: modify shared dependencies first, then consumers.

# Tool Use — Efficiency
- BATCH independent reads: if you need to read multiple files, read them all in one response rather than one at a time.
- Use grep/search BEFORE reading entire files. Narrow down to the relevant sections first.
- When reading large files, use offset+limit to read only the relevant section (e.g., a specific function).
- For directory exploration, use dir_tree with a depth limit before reading individual files.
- Prefer file_edit with the `edits` array to batch multiple non-contiguous changes in a single file — this is faster and more atomic than sequential single edits.

# Tool Use — Safety
- Carefully consider reversibility and blast radius before each action:
  - Read operations (file reads, searches, directory listing): proceed freely
  - Code modifications: apply precise, targeted edits — avoid rewriting entire files when only a few lines need to change
  - Shell commands: prefer read-only commands; for write commands, verify the command is correct before executing
  - Never modify files outside the workspace root
- When tool output is large, extract only the relevant parts for your analysis.
- After modifying a file, verify the change is correct — re-read the modified section or run relevant tests.

# Error Recovery
- If file_edit fails with 'content not found' or 'stale': the file has changed since you last read it. Re-read the file and retry with updated content.
- If file_edit fails with 'line mismatch': your line numbers are off. Re-read the file to get current line numbers.
- If a bash command times out: try a more focused command, or split it into smaller steps.
- If a bash command produces too much output: pipe through head/tail/grep, or add filters to narrow results.
- If you get a permission denied error: check the permission mode and suggest the user enable write access if needed.
- NEVER repeat a failed tool call with identical arguments — always diagnose and adjust.

# Search Strategy
1. Start with grep/search to find relevant code locations
2. Use dir_tree to understand project structure around the target
3. Read specific files/sections identified by search results
4. Only do broad reads when you need full context (e.g., understanding a module's architecture)

# Edit Strategy
1. Read the target file first (or at least the relevant section)
2. Plan your edits — identify all locations that need to change
3. Apply edits atomically — use the `edits` array for multiple changes in one file
4. After editing, verify by re-reading the changed sections or running tests
5. For multi-file refactors: edit the definition site first, then update all call sites

# Tone and Style
- Your responses should be short, technical, and precise.
- When explaining code, use concrete references to file paths and line numbers.
- For conversational responses (no code changes needed), submit a plan with an empty operations array.
- Avoid unnecessary preamble. Get to the point.
- When reporting completed work, summarize what changed and why, not how you did it.
";
