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
        self.branch.is_none() && self.recent_commits.is_empty() && self.status.is_none()
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

        format!("\n## Git Context\n\n{}\n", sections.join("\n\n"))
    }

    /// Render git context as XML-tagged block (Codex `environment_context` pattern).
    ///
    /// Used for structured injection into system prompts, enabling the agent
    /// to parse context boundaries cleanly.
    pub fn render_xml(&self) -> String {
        if self.is_empty() {
            return String::new();
        }

        let mut out = String::with_capacity(512);
        out.push_str("<git_context>\n");

        if let Some(ref branch) = self.branch {
            out.push_str(&format!("  <branch>{branch}</branch>\n"));
        }

        if let Some(ref status) = self.status {
            let trimmed = status.trim();
            if !trimmed.is_empty() {
                let clean = !trimmed.lines().skip(1).any(|l| !l.is_empty());
                out.push_str(&format!(
                    "  <working_tree clean=\"{clean}\">{trimmed}</working_tree>\n"
                ));
            }
        }

        if !self.recent_commits.is_empty() {
            out.push_str("  <recent_commits>\n");
            for commit in &self.recent_commits {
                out.push_str(&format!("    <commit>{commit}</commit>\n"));
            }
            out.push_str("  </recent_commits>\n");
        }

        out.push_str("</git_context>");
        out
    }
}

/// Lightweight git status summary for quick checks (Codex pattern).
///
/// Unlike `GitContext`, this only captures branch and clean/dirty status
/// without running expensive diff commands. Useful for subagent context
/// injection where full diffs are not needed.
#[derive(Debug, Clone)]
pub struct GitStatusSummary {
    /// Branch name.
    pub branch: String,
    /// Whether the working tree is clean.
    pub is_clean: bool,
    /// Number of modified files.
    pub modified_count: usize,
    /// Number of untracked files.
    pub untracked_count: usize,
}

impl GitStatusSummary {
    /// Detect a lightweight git status summary.
    ///
    /// Returns `None` if not inside a git repository.
    pub fn detect(workspace_root: &Path) -> Option<Self> {
        let branch = run_git(workspace_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let status_output = run_git(workspace_root, &["status", "--porcelain"]).unwrap_or_default();

        let modified_count = status_output
            .lines()
            .filter(|l| l.starts_with(" M") || l.starts_with("M ") || l.starts_with("MM"))
            .count();
        let untracked_count = status_output
            .lines()
            .filter(|l| l.starts_with("??"))
            .count();
        let is_clean = status_output.trim().is_empty();

        Some(Self {
            branch,
            is_clean,
            modified_count,
            untracked_count,
        })
    }

    /// One-line summary suitable for logging or status bar display.
    pub fn one_line(&self) -> String {
        if self.is_clean {
            format!("🌿 {} (clean)", self.branch)
        } else {
            format!(
                "🌿 {} ({} modified, {} untracked)",
                self.branch, self.modified_count, self.untracked_count
            )
        }
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
