//! Agent Role system (Codex `agent/role.rs` pattern).
//!
//! Defines configurable agent personas that control behavior, permissions,
//! and resource allocation. Each role is a layered configuration overlay
//! that merges with the base agent config at spawn time.
//!
//! # Built-in Roles
//!
//! | Role | Purpose | Restrictions |
//! |------|---------|-------------|
//! | `default` | General-purpose agent | None |
//! | `explorer` | Read-only reconnaissance | No writes, low reasoning effort |
//! | `worker` | Focused execution | File ownership, concurrent-safe |
//! | `coder` | Code generation specialist | Full write access |
//! | `reviewer` | Code review agent | Read-only, high reasoning effort |
//!
//! # Usage
//!
//! ```ignore
//! let role = AgentRole::builtin("explorer");
//! let config = role.apply_to(base_config);
//! ```

use std::collections::HashMap;
use std::time::Duration;

/// An agent role defines behavioral constraints and configuration overrides.
#[derive(Debug, Clone)]
pub struct AgentRole {
    /// Unique role identifier (e.g., "explorer", "worker", "coder").
    pub name: String,

    /// Human-readable description of the role's purpose.
    pub description: String,

    /// System prompt additions injected when this role is active.
    pub system_prompt_suffix: String,

    /// Permission level for this role.
    pub permission_level: RolePermissionLevel,

    /// Maximum turn duration before the agent is forcibly stopped.
    pub max_turn_duration: Duration,

    /// Reasoning effort hint for the LLM (maps to provider-specific params).
    pub reasoning_effort: ReasoningEffort,

    /// Maximum number of tool calls per turn.
    pub max_tool_calls_per_turn: usize,

    /// Maximum agent loop iterations.
    pub max_steps: usize,

    /// Whether this role can delegate to sub-agents.
    pub can_delegate: bool,

    /// File path patterns this role is allowed to modify (empty = all).
    pub file_ownership: Vec<String>,

    /// Custom metadata for role-specific behavior.
    pub metadata: HashMap<String, String>,
}

/// Permission level for a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolePermissionLevel {
    /// Read-only access — no file writes, no destructive commands.
    ReadOnly,
    /// Workspace-scoped writes.
    WorkspaceWrite,
    /// Full access.
    FullAccess,
}

/// Reasoning effort level hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    /// Minimal reasoning — fast, shallow responses.
    Low,
    /// Standard reasoning depth.
    Medium,
    /// Deep reasoning — slower but more thorough.
    High,
}

impl Default for AgentRole {
    fn default() -> Self {
        Self::builtin("default")
    }
}

impl AgentRole {
    /// Create a built-in role by name.
    ///
    /// Returns the `default` role for unrecognized names.
    pub fn builtin(name: &str) -> Self {
        match name {
            "explorer" => Self::explorer(),
            "worker" => Self::worker(),
            "coder" => Self::coder(),
            "reviewer" => Self::reviewer(),
            _ => Self::default_role(),
        }
    }

    fn default_role() -> Self {
        Self {
            name: "default".to_string(),
            description: "General-purpose agent".to_string(),
            system_prompt_suffix: String::new(),
            permission_level: RolePermissionLevel::WorkspaceWrite,
            max_turn_duration: Duration::from_secs(300),
            reasoning_effort: ReasoningEffort::Medium,
            max_tool_calls_per_turn: 20,
            max_steps: 40,
            can_delegate: true,
            file_ownership: vec![],
            metadata: HashMap::new(),
        }
    }

    fn explorer() -> Self {
        Self {
            name: "explorer".to_string(),
            description: "Read-only reconnaissance agent. Explores codebase structure, reads files, runs searches. Cannot modify any files.".to_string(),
            system_prompt_suffix: "You are an Explorer agent. Your job is ONLY to gather information. \
                You MUST NOT write, edit, or delete any files. Use only read-only tools: \
                read_file, grep, glob, list_dir, bash (read-only commands only). \
                Be thorough but efficient — minimize token usage.".to_string(),
            permission_level: RolePermissionLevel::ReadOnly,
            max_turn_duration: Duration::from_secs(120),
            reasoning_effort: ReasoningEffort::Low,
            max_tool_calls_per_turn: 30, // explorers read a lot
            max_steps: 20,
            can_delegate: false,
            file_ownership: vec![],
            metadata: HashMap::new(),
        }
    }

    fn worker() -> Self {
        Self {
            name: "worker".to_string(),
            description: "Focused execution agent. Implements specific changes within assigned file ownership boundaries.".to_string(),
            system_prompt_suffix: "You are a Worker agent. Focus on implementing the specific task assigned to you. \
                Work within your assigned file boundaries. Be precise and avoid unnecessary changes.".to_string(),
            permission_level: RolePermissionLevel::WorkspaceWrite,
            max_turn_duration: Duration::from_secs(300),
            reasoning_effort: ReasoningEffort::Medium,
            max_tool_calls_per_turn: 20,
            max_steps: 40,
            can_delegate: false,
            file_ownership: vec![],
            metadata: HashMap::new(),
        }
    }

    fn coder() -> Self {
        Self {
            name: "coder".to_string(),
            description: "Code generation specialist. Full write access with deep reasoning for complex implementations.".to_string(),
            system_prompt_suffix: "You are a Coder agent. You have full write access to the workspace. \
                Think carefully about architecture and implementation before making changes. \
                Write clean, well-documented code.".to_string(),
            permission_level: RolePermissionLevel::FullAccess,
            max_turn_duration: Duration::from_secs(600),
            reasoning_effort: ReasoningEffort::High,
            max_tool_calls_per_turn: 20,
            max_steps: 60,
            can_delegate: true,
            file_ownership: vec![],
            metadata: HashMap::new(),
        }
    }

    fn reviewer() -> Self {
        Self {
            name: "reviewer".to_string(),
            description: "Code review agent. Read-only access with high reasoning effort for thorough analysis.".to_string(),
            system_prompt_suffix: "You are a Reviewer agent. Analyze code changes for correctness, \
                security issues, performance problems, and style violations. \
                You MUST NOT modify any files — only report your findings.".to_string(),
            permission_level: RolePermissionLevel::ReadOnly,
            max_turn_duration: Duration::from_secs(180),
            reasoning_effort: ReasoningEffort::High,
            max_tool_calls_per_turn: 15,
            max_steps: 20,
            can_delegate: false,
            file_ownership: vec![],
            metadata: HashMap::new(),
        }
    }

    /// Assign file ownership to this role (for worker agents).
    pub fn with_file_ownership(mut self, patterns: Vec<String>) -> Self {
        self.file_ownership = patterns;
        self
    }

    /// Check if a file path is owned by this role.
    /// Returns true if no ownership is configured (unrestricted).
    pub fn owns_file(&self, path: &str) -> bool {
        if self.file_ownership.is_empty() {
            return true;
        }
        self.file_ownership.iter().any(|pattern| {
            // Simple prefix matching — could be upgraded to glob patterns
            path.starts_with(pattern) || path.contains(pattern)
        })
    }

    /// List all available built-in role names.
    pub fn builtin_names() -> &'static [&'static str] {
        &["default", "explorer", "worker", "coder", "reviewer"]
    }
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.description)
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_roles() {
        for name in AgentRole::builtin_names() {
            let role = AgentRole::builtin(name);
            assert_eq!(role.name, *name);
            assert!(!role.description.is_empty());
        }
    }

    #[test]
    fn test_explorer_is_read_only() {
        let role = AgentRole::builtin("explorer");
        assert_eq!(role.permission_level, RolePermissionLevel::ReadOnly);
        assert!(!role.can_delegate);
    }

    #[test]
    fn test_file_ownership() {
        let role = AgentRole::builtin("worker")
            .with_file_ownership(vec!["crates/crow-brain/".to_string()]);
        assert!(role.owns_file("crates/crow-brain/src/client.rs"));
        assert!(!role.owns_file("crates/crow-cli/src/main.rs"));
    }

    #[test]
    fn test_unknown_role_defaults() {
        let role = AgentRole::builtin("nonexistent");
        assert_eq!(role.name, "default");
    }
}
