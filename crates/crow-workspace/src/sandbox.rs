use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Frictionless execution environment using Git for rollbacks.
pub struct GitWorkspaceManager {
    root: PathBuf,
    original_branch: String,
    auto_branch: String,
}

impl GitWorkspaceManager {
    pub async fn new(root: &Path) -> Result<Self> {
        let branch_output = Command::new("git")
            .current_dir(root)
            .args(["branch", "--show-current"])
            .output()
            .await?;

        let original_branch = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string();
        if original_branch.is_empty() {
            anyhow::bail!("Workspace must be a valid git repository with an active branch");
        }

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_micros();
        let auto_branch = format!("crow-auto-branch-{:08x}", ts as u32);

        // Stash any uncommitted changes just in case
        let _ = Command::new("git")
            .current_dir(root)
            .args(["stash", "push", "-u", "-m", "crow-auto-stash"])
            .output()
            .await?;

        let checkout_res = Command::new("git")
            .current_dir(root)
            .args(["checkout", "-b", &auto_branch])
            .output()
            .await?;

        if !checkout_res.status.success() {
            anyhow::bail!("Failed to create temporary git branch {auto_branch}");
        }

        Ok(Self {
            root: root.to_path_buf(),
            original_branch,
            auto_branch,
        })
    }

    /// Rollback the workspace to the original branch and restore stashed changes.
    pub async fn rollback(&self) -> Result<()> {
        Command::new("git")
            .current_dir(&self.root)
            .args(["reset", "--hard"])
            .output()
            .await?;

        Command::new("git")
            .current_dir(&self.root)
            .args(["checkout", &self.original_branch])
            .output()
            .await?;

        let _ = Command::new("git")
            .current_dir(&self.root)
            .args(["branch", "-D", &self.auto_branch])
            .output()
            .await?;

        // Pop stash if it was ours (simplified)
        let stash_list = Command::new("git")
            .current_dir(&self.root)
            .args(["stash", "list"])
            .output()
            .await?;

        if String::from_utf8_lossy(&stash_list.stdout).contains("crow-auto-stash") {
            let _ = Command::new("git")
                .current_dir(&self.root)
                .args(["stash", "pop"])
                .output()
                .await?;
        }

        Ok(())
    }

    /// Commit the successful branch and merge back.
    pub async fn commit_and_merge(&self) -> Result<()> {
        Command::new("git")
            .current_dir(&self.root)
            .args(["add", "."])
            .output()
            .await?;

        Command::new("git")
            .current_dir(&self.root)
            .args(["commit", "-m", "crow-auto-commit (verification passed)"])
            .output()
            .await?;

        Command::new("git")
            .current_dir(&self.root)
            .args(["checkout", &self.original_branch])
            .output()
            .await?;

        Command::new("git")
            .current_dir(&self.root)
            .args(["merge", "--ff-only", &self.auto_branch])
            .output()
            .await?;

        let _ = Command::new("git")
            .current_dir(&self.root)
            .args(["branch", "-d", &self.auto_branch])
            .output()
            .await?;

        Ok(())
    }
}
