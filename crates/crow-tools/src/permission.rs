//! Permission Enforcement Layer.
//!
//! Enforces access controls for tool executions. Ported from claw-code's
//! `permission_enforcer.rs` with granular `PermissionMode` gating and
//! integration with the `bash_validation` engine.

use crate::bash_validation::{self, ValidationResult};
use std::path::Path;

/// Permission modes controlling what operations the agent can perform.
///
/// Ordered from most restrictive to least restrictive.
/// Inspired by claw-code's `PermissionMode` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// Read-only: no file writes, no destructive commands, no system mutation.
    /// Only observational tools (ls, cat, grep, find, git status, etc.) are allowed.
    ReadOnly,
    /// Workspace-scoped writes: file mutations within the workspace boundary
    /// are allowed. Destructive commands produce warnings. System-level
    /// commands are blocked.
    #[default]
    WorkspaceWrite,
    /// Interactive prompt mode: same as WorkspaceWrite but destructive
    /// commands require user confirmation via the TUI approval dialog.
    Prompt,
    /// Full access with no restrictions. Equivalent to the legacy "YOLO" mode.
    /// All commands execute without validation. Use only when the operator
    /// explicitly opts in via `CROW_WRITE_MODE=danger`.
    DangerFullAccess,
}

/// Legacy write mode for backward compatibility.
/// Maps to the new `PermissionMode` internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteMode {
    Safe,
    #[default]
    Sandbox,
    Danger,
}

impl From<WriteMode> for PermissionMode {
    fn from(wm: WriteMode) -> Self {
        match wm {
            WriteMode::Safe => PermissionMode::ReadOnly,
            WriteMode::Sandbox => PermissionMode::WorkspaceWrite,
            WriteMode::Danger => PermissionMode::DangerFullAccess,
        }
    }
}

impl From<PermissionMode> for WriteMode {
    fn from(pm: PermissionMode) -> Self {
        match pm {
            PermissionMode::ReadOnly => WriteMode::Safe,
            PermissionMode::WorkspaceWrite | PermissionMode::Prompt => WriteMode::Sandbox,
            PermissionMode::DangerFullAccess => WriteMode::Danger,
        }
    }
}

pub struct PermissionEnforcer {
    pub mode: WriteMode,
    pub permission_mode: PermissionMode,
    pub workspace_root: std::path::PathBuf,
}

impl Default for PermissionEnforcer {
    fn default() -> Self {
        Self::new(WriteMode::Sandbox)
    }
}

impl PermissionEnforcer {
    pub fn new(mode: WriteMode) -> Self {
        Self {
            permission_mode: mode.into(),
            mode,
            workspace_root: std::path::PathBuf::new(),
        }
    }

    /// Create with explicit permission mode and workspace root.
    pub fn with_workspace(mode: PermissionMode, workspace_root: std::path::PathBuf) -> Self {
        Self {
            mode: mode.into(),
            permission_mode: mode,
            workspace_root,
        }
    }

    /// Check if a destructive file write operation is allowed.
    pub fn check_file_write(&self, path: &Path) -> Result<(), anyhow::Error> {
        match self.permission_mode {
            PermissionMode::ReadOnly => anyhow::bail!("Write denied: running in read-only mode"),
            PermissionMode::WorkspaceWrite | PermissionMode::Prompt => {
                // Validate workspace boundary
                self.check_workspace_boundary(path)?;
                Ok(())
            }
            PermissionMode::DangerFullAccess => Ok(()),
        }
    }

    /// Check if a bash command is allowed using the full validation pipeline.
    pub fn check_bash(&self, cmd: &str) -> Result<(), anyhow::Error> {
        let result =
            bash_validation::validate_command(cmd, self.permission_mode, &self.workspace_root);

        match result {
            ValidationResult::Allow => Ok(()),
            ValidationResult::Block { reason } => anyhow::bail!("{reason}"),
            ValidationResult::Warn { message } => {
                // In Prompt mode, this would go through the TUI approval dialog.
                // For now, warnings are logged but allowed (matching legacy behavior).
                // The TUI layer can intercept Warn results for interactive approval.
                tracing::warn!("{message}");
                Ok(())
            }
        }
    }

    /// Check if a file path is within the workspace boundary.
    ///
    /// Resolves symlinks and `../` traversal to detect escape attempts.
    pub fn check_workspace_boundary(&self, path: &Path) -> Result<(), anyhow::Error> {
        if self.workspace_root.as_os_str().is_empty() {
            // No workspace root configured — skip boundary check
            return Ok(());
        }

        if self.permission_mode == PermissionMode::DangerFullAccess {
            return Ok(());
        }

        // Resolve to absolute path for comparison
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        };

        // Canonicalize both paths for accurate comparison.
        // If the path doesn't exist yet (e.g., new file creation), check the parent.
        let canonical_ws = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());
        let canonical_target = resolved
            .canonicalize()
            .or_else(|_| {
                // File doesn't exist yet — check the parent directory
                resolved
                    .parent()
                    .and_then(|p| p.canonicalize().ok())
                    .map(|p| p.join(resolved.file_name().unwrap_or_default()))
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no parent"))
            })
            .unwrap_or(resolved);

        if !canonical_target.starts_with(&canonical_ws) {
            anyhow::bail!(
                "Path '{}' escapes workspace boundary '{}'",
                canonical_target.display(),
                canonical_ws.display()
            );
        }

        Ok(())
    }

    /// Return the current permission mode's intent classification for a command.
    ///
    /// Useful for the TUI to display what kind of operation is being attempted.
    pub fn classify_command(&self, cmd: &str) -> bash_validation::CommandIntent {
        bash_validation::classify_intent(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_blocks_writes() {
        let enforcer = PermissionEnforcer::with_workspace(
            PermissionMode::ReadOnly,
            std::path::PathBuf::from("/tmp"),
        );
        assert!(enforcer
            .check_file_write(std::path::Path::new("/tmp/foo.txt"))
            .is_err());
    }

    #[test]
    fn workspace_write_allows_within_boundary() {
        let enforcer = PermissionEnforcer::with_workspace(
            PermissionMode::WorkspaceWrite,
            std::path::PathBuf::from("/tmp"),
        );
        // /tmp/foo.txt is within /tmp, should be allowed
        assert!(enforcer
            .check_file_write(std::path::Path::new("/tmp/foo.txt"))
            .is_ok());
    }

    #[test]
    fn workspace_write_blocks_outside_boundary() {
        let enforcer = PermissionEnforcer::with_workspace(
            PermissionMode::WorkspaceWrite,
            std::path::PathBuf::from("/tmp/myproject"),
        );
        // /etc/passwd is outside /tmp/myproject
        let result = enforcer.check_workspace_boundary(std::path::Path::new("/etc/passwd"));
        assert!(result.is_err());
    }

    #[test]
    fn danger_allows_everything() {
        let enforcer = PermissionEnforcer::with_workspace(
            PermissionMode::DangerFullAccess,
            std::path::PathBuf::from("/tmp"),
        );
        assert!(enforcer
            .check_file_write(std::path::Path::new("/etc/passwd"))
            .is_ok());
        assert!(enforcer.check_bash("rm -rf /").is_ok());
    }

    #[test]
    fn read_only_blocks_destructive_bash() {
        let enforcer = PermissionEnforcer::with_workspace(
            PermissionMode::ReadOnly,
            std::path::PathBuf::from("/tmp"),
        );
        assert!(enforcer.check_bash("rm -rf /tmp").is_err());
    }

    #[test]
    fn read_only_allows_safe_bash() {
        let enforcer = PermissionEnforcer::with_workspace(
            PermissionMode::ReadOnly,
            std::path::PathBuf::from("/tmp"),
        );
        assert!(enforcer.check_bash("ls -la").is_ok());
        assert!(enforcer.check_bash("cat foo.rs").is_ok());
        assert!(enforcer.check_bash("cargo test").is_ok());
    }

    #[test]
    fn backward_compat_write_mode() {
        let enforcer = PermissionEnforcer::new(WriteMode::Safe);
        assert_eq!(enforcer.permission_mode, PermissionMode::ReadOnly);
        assert!(enforcer.check_bash("rm foo").is_err());

        let enforcer = PermissionEnforcer::new(WriteMode::Danger);
        assert_eq!(enforcer.permission_mode, PermissionMode::DangerFullAccess);
        assert!(enforcer.check_bash("rm -rf /").is_ok());
    }
}
