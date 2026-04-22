//! Permission Enforcement Layer.
//!
//! Enforces access controls for tool executions.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteMode {
    Safe,
    #[default]
    Sandbox,
    Danger,
}

pub struct PermissionEnforcer {
    pub mode: WriteMode,
}

impl Default for PermissionEnforcer {
    fn default() -> Self {
        Self::new(WriteMode::Sandbox)
    }
}

impl PermissionEnforcer {
    pub fn new(mode: WriteMode) -> Self {
        Self { mode }
    }

    /// Check if a destructive file write operation is allowed.
    pub fn check_file_write(&self, _path: &Path) -> Result<(), anyhow::Error> {
        match self.mode {
            WriteMode::Safe => anyhow::bail!("Write denied: running in Safe mode"),
            WriteMode::Sandbox | WriteMode::Danger => Ok(()), // Sandbox will catch it at L1
        }
    }

    /// Check if a bash command is allowed.
    pub fn check_bash(&self, cmd: &str) -> Result<(), anyhow::Error> {
        match self.mode {
            WriteMode::Safe => {
                // Heuristic read-only check (similar to claw-code)
                let cmd_lower = cmd.to_lowercase();
                if cmd_lower.contains("rm ") || cmd_lower.contains("mv ") || cmd_lower.contains(">") {
                    anyhow::bail!("Destructive command denied in Safe mode");
                }
                Ok(())
            }
            WriteMode::Sandbox | WriteMode::Danger => Ok(()),
        }
    }
}
