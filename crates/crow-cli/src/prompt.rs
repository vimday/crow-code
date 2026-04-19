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
            identity: "You are an autonomous engineering agent executing the given task.".to_string(),
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

    pub fn with_mcp(mut self, mcp_manager: Option<&crate::mcp::McpManager>) -> Self {
        if let Some(mgr) = mcp_manager {
            let mcp_ctx = mgr.prompt_context();
            if !mcp_ctx.is_empty() {
                self.skills.push_str("MCP Interface:\n");
                self.skills.push_str(mcp_ctx);
            }
        }
        self
    }

    pub fn with_contract(mut self, snapshot_id: &SnapshotId) -> Self {
        self.contract = format!(
            "IMPORTANT: When you submit a plan, set base_snapshot_id to \"{}\" exactly.\n\n\
            Constraints: Please limit your edits to Create and Modify operations if possible for this early iteration.\n\n\
            MCTS DYNAMIC SEARCH: For complex code refactors, we use rigorous parallel searches (MCTS). \
            However, if your intended changes are TRIVIAL (e.g. pure documentation tweaks, simple text formatting, \
            or modifying markdown files), please explicitly set `requires_mcts = false` to save precious API loop latency.",
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
