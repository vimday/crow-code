//! Permission-aware prompt instructions (Codex `permissions_instructions.rs` pattern).
//!
//! Generates structured text that tells the agent exactly what it can and
//! cannot do within its current permission mode. This replaces the silent
//! denial pattern where the agent discovers permissions only when a tool
//! call is rejected.
//!
//! # Architecture
//!
//! The prompt text is parameterized by `PermissionMode` and injected into
//! the system prompt via `SystemPromptBuilder::with_permissions_prompt()`.
//!
//! ```text
//! <permissions instructions>
//! # Sandbox Policy: workspace_write
//! You have write access to the workspace directory...
//! </permissions instructions>
//! ```

/// Permission mode (mirrors crow_tools::PermissionMode).
///
/// Duplicated here to avoid a crate dependency cycle. The agent loop
/// maps `crow_tools::PermissionMode` → `PromptPermissionMode` at turn start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptPermissionMode {
    /// Full unrestricted access — all tools auto-approved.
    DangerFullAccess,
    /// Write access scoped to the workspace directory.
    WorkspaceWrite,
    /// Read-only — no file writes, no destructive commands.
    ReadOnly,
}

/// Structured permission instructions for the system prompt.
#[derive(Debug, Clone)]
pub struct PermissionsPrompt {
    mode: PromptPermissionMode,
    workspace_root: String,
}

impl PermissionsPrompt {
    /// Create permission instructions for the given mode and workspace.
    pub fn new(mode: PromptPermissionMode, workspace_root: impl Into<String>) -> Self {
        Self {
            mode,
            workspace_root: workspace_root.into(),
        }
    }

    /// Render the structured permission instructions text.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("<permissions_instructions>\n");

        match self.mode {
            PromptPermissionMode::DangerFullAccess => {
                out.push_str("# Sandbox Policy: full_access\n\n");
                out.push_str(
                    "You have FULL unrestricted access to the filesystem and network.\n\
                     All tool calls are auto-approved without user confirmation.\n\
                     You may read, write, and delete any files on the system.\n\
                     You may execute any shell commands.\n\n",
                );
                out.push_str("## Tool Usage Policy\n\n");
                out.push_str(
                    "- Use `file_edit` and `file_write` for file modifications\n\
                     - Use `bash` for shell commands (no restrictions)\n\
                     - Use `read_file`, `grep`, `glob`, `list_dir` for reconnaissance\n\
                     - Use `delegate` to spawn sub-agents for parallel work\n",
                );
            }
            PromptPermissionMode::WorkspaceWrite => {
                out.push_str("# Sandbox Policy: workspace_write\n\n");
                out.push_str(&format!(
                    "You have write access scoped to the workspace directory: `{}`\n\
                     File operations outside this directory will be denied.\n\
                     Shell commands that modify files outside the workspace may be blocked.\n\n",
                    self.workspace_root
                ));
                out.push_str("## Tool Usage Policy\n\n");
                out.push_str(
                    "- Use `file_edit` and `file_write` for workspace file modifications\n\
                     - Use `bash` for shell commands (workspace-scoped writes only)\n\
                     - Use `read_file`, `grep`, `glob`, `list_dir` for reconnaissance\n\
                     - Avoid modifying files outside the workspace root\n",
                );
            }
            PromptPermissionMode::ReadOnly => {
                out.push_str("# Sandbox Policy: read_only\n\n");
                out.push_str(
                    "You are in READ-ONLY mode. You MUST NOT write, edit, or delete any files.\n\
                     Only read-only tools are available: `read_file`, `grep`, `glob`, `list_dir`.\n\
                     Shell commands via `bash` are restricted to read-only operations.\n\
                     Any attempt to use `file_edit`, `file_write`, or destructive `bash` commands \
                     will be denied.\n\n",
                );
                out.push_str("## Tool Usage Policy\n\n");
                out.push_str(
                    "- Use `read_file` to examine file contents\n\
                     - Use `grep` and `glob` for searching\n\
                     - Use `list_dir` for directory exploration\n\
                     - Use `bash` ONLY for read-only commands (e.g., `cat`, `find`, `wc`)\n\
                     - Do NOT attempt `file_edit`, `file_write`, or `rm`/`mv`/`cp` via bash\n",
                );
            }
        }

        out.push_str("</permissions_instructions>");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_access_prompt() {
        let prompt = PermissionsPrompt::new(
            PromptPermissionMode::DangerFullAccess,
            "/workspace",
        );
        let rendered = prompt.render();
        assert!(rendered.contains("full_access"));
        assert!(rendered.contains("unrestricted"));
        assert!(rendered.contains("<permissions_instructions>"));
        assert!(rendered.contains("</permissions_instructions>"));
    }

    #[test]
    fn test_workspace_write_prompt() {
        let prompt = PermissionsPrompt::new(
            PromptPermissionMode::WorkspaceWrite,
            "/home/user/project",
        );
        let rendered = prompt.render();
        assert!(rendered.contains("workspace_write"));
        assert!(rendered.contains("/home/user/project"));
    }

    #[test]
    fn test_read_only_prompt() {
        let prompt = PermissionsPrompt::new(
            PromptPermissionMode::ReadOnly,
            "/workspace",
        );
        let rendered = prompt.render();
        assert!(rendered.contains("read_only"));
        assert!(rendered.contains("READ-ONLY"));
        assert!(rendered.contains("MUST NOT write"));
    }
}
