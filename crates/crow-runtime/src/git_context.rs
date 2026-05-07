//! Git context injection for system prompts (Claw-Code pattern).
//!
//! Automatically detects the current git branch, recent commits, staged/unstaged
//! changes, and status — then renders them as a formatted section that the
//! `SystemPromptBuilder` can inject into the system prompt.
//!
//! This gives the agent much better situational awareness about the current
//! state of the repository without requiring the user to manually describe it.

use std::path::Path;
use std::process::Command;

/// Maximum bytes for the diff section to prevent context bloat.
const MAX_DIFF_BYTES: usize = 8 * 1024; // 8 KB

/// Maximum number of recent commits to include.
const MAX_RECENT_COMMITS: usize = 5;

/// Captured git context for the current workspace.
#[derive(Debug, Clone, Default)]
pub struct GitContext {
    /// Current branch name (e.g. "main", "feature/foo").
    pub branch: Option<String>,
    /// Short log of recent commits (most recent first).
    pub recent_commits: Vec<String>,
    /// `git status --short` output.
    pub status: Option<String>,
    /// Staged diff (git diff --cached), truncated to budget.
    pub staged_diff: Option<String>,
    /// Unstaged diff (git diff), truncated to budget.
    pub unstaged_diff: Option<String>,
}

impl GitContext {
    /// Detect git context from the workspace root.
    ///
    /// Returns `None` if the workspace is not a git repository.
    /// Individual fields may be `None` if the corresponding git command fails.
    pub fn detect(workspace_root: &Path) -> Option<Self> {
        // Quick check: is this a git repo?
        let is_git = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(workspace_root)
            .output()
            .ok()
            .is_some_and(|o| o.status.success());

        if !is_git {
            return None;
        }

        let branch = run_git(workspace_root, &["rev-parse", "--abbrev-ref", "HEAD"]);

        let recent_commits = run_git(
            workspace_root,
            &[
                "log",
                &format!("-{MAX_RECENT_COMMITS}"),
                "--oneline",
                "--no-decorate",
            ],
        )
        .map(|s| s.lines().map(String::from).collect::<Vec<_>>())
        .unwrap_or_default();

        let status = run_git(workspace_root, &["status", "--short", "--branch"]);

        let staged_diff = run_git(workspace_root, &["diff", "--cached", "--stat"])
            .map(|s| truncate_to_budget(&s, MAX_DIFF_BYTES));

        let unstaged_diff = run_git(workspace_root, &["diff", "--stat"])
            .map(|s| truncate_to_budget(&s, MAX_DIFF_BYTES));

        Some(Self {
            branch,
            recent_commits,
            status,
            staged_diff,
            unstaged_diff,
        })
    }

    /// Whether this context has any meaningful content to inject.
    pub fn is_empty(&self) -> bool {
        self.branch.is_none()
            && self.recent_commits.is_empty()
            && self.status.is_none()
    }

    /// Render the git context as a formatted section for the system prompt.
    pub fn render(&self) -> String {
        if self.is_empty() {
            return String::new();
        }

        let mut sections = Vec::new();

        if let Some(ref branch) = self.branch {
            sections.push(format!("Current branch: {branch}"));
        }

        if let Some(ref status) = self.status {
            let trimmed = status.trim();
            if !trimmed.is_empty() {
                sections.push(format!("Git status:\n```\n{trimmed}\n```"));
            }
        }

        if !self.recent_commits.is_empty() {
            let commits = self.recent_commits.join("\n");
            sections.push(format!("Recent commits:\n```\n{commits}\n```"));
        }

        if let Some(ref diff) = self.staged_diff {
            let trimmed = diff.trim();
            if !trimmed.is_empty() {
                sections.push(format!("Staged changes:\n```\n{trimmed}\n```"));
            }
        }

        if let Some(ref diff) = self.unstaged_diff {
            let trimmed = diff.trim();
            if !trimmed.is_empty() {
                sections.push(format!("Unstaged changes:\n```\n{trimmed}\n```"));
            }
        }

        if sections.is_empty() {
            return String::new();
        }

        format!(
            "\n## Git Context\n\n{}\n",
            sections.join("\n\n")
        )
    }
}

/// Run a git command and return its trimmed stdout, or `None` on failure.
fn run_git(workspace_root: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(workspace_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Truncate content to a byte budget, appending a warning if truncated.
fn truncate_to_budget(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let truncated = crow_patch::safe_truncate(content, max_bytes);
    format!("{truncated}\n\n[truncated — diff exceeds {max_bytes} byte budget]")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_none_for_non_git_dir() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let ctx = GitContext::detect(tmp.path());
        assert!(ctx.is_none());
    }

    #[test]
    fn render_empty_context_returns_empty_string() {
        let ctx = GitContext::default();
        assert!(ctx.render().is_empty());
    }

    #[test]
    fn render_with_branch_only() {
        let ctx = GitContext {
            branch: Some("main".into()),
            ..Default::default()
        };
        let rendered = ctx.render();
        assert!(rendered.contains("Current branch: main"));
        assert!(rendered.contains("## Git Context"));
    }

    #[test]
    fn truncate_to_budget_preserves_small_content() {
        let small = "hello world";
        assert_eq!(truncate_to_budget(small, 1000), small);
    }

    #[test]
    fn truncate_to_budget_truncates_large_content() {
        let large = "x".repeat(2000);
        let result = truncate_to_budget(&large, 100);
        assert!(result.contains("[truncated"));
        assert!(result.len() < large.len());
    }
}
