//! Core types for the verifier contract.

use crow_evidence::TestRun;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

// ─── ACI Configuration ─────────────────────────────────────────────

/// Configuration for Adaptive Context Integration (ACI) log truncation.
///
/// Raw command output can be megabytes. The LLM's context window is finite.
/// ACI keeps the most diagnostic-value lines: the first N lines (compilation
/// errors, test headers) and the last M lines (summary, exit status).
#[derive(Debug, Clone)]
pub struct AciConfig {
    /// Maximum number of lines to retain in truncated output.
    pub max_lines: usize,
    /// Number of lines to keep from the beginning (errors, headers).
    pub head_lines: usize,
    /// Number of lines to keep from the end (summaries, totals).
    pub tail_lines: usize,
}

impl AciConfig {
    /// Sensible defaults: 200 total lines, 50 head + 150 tail.
    pub fn default_config() -> Self {
        Self {
            max_lines: 200,
            head_lines: 50,
            tail_lines: 150,
        }
    }

    /// Compact config for tight token budgets.
    pub fn compact() -> Self {
        Self {
            max_lines: 80,
            head_lines: 20,
            tail_lines: 60,
        }
    }

    /// Validate that head + tail <= max_lines.
    pub fn validate(&self) -> Result<(), String> {
        if self.head_lines + self.tail_lines > self.max_lines {
            return Err(format!(
                "head_lines ({}) + tail_lines ({}) exceeds max_lines ({})",
                self.head_lines, self.tail_lines, self.max_lines
            ));
        }
        Ok(())
    }
}

// ─── Execution Configuration ────────────────────────────────────────

/// Configuration for command execution.
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum wall-clock time before the command is killed.
    pub timeout: Duration,
    /// Maximum bytes to capture from stdout+stderr combined.
    /// Prevents OOM from pathological output.
    pub max_output_bytes: usize,
}

impl ExecutionConfig {
    /// Sensible defaults: 5 minutes, 10 MiB capture limit.
    pub fn default_config() -> Self {
        Self {
            timeout: Duration::from_secs(300),
            max_output_bytes: 10 * 1024 * 1024,
        }
    }
}

// ─── Verification Result ────────────────────────────────────────────

/// The complete result of running a verification command in a sandbox.
#[derive(Debug, Clone)]
pub struct VerificationResult {
    /// Structured test run result (feeds into `EvidenceMatrix`).
    pub test_run: TestRun,
    /// Process exit code. `None` if killed by timeout or signal.
    pub exit_code: Option<i32>,
    /// Number of bytes actually retained within the buffer cap.
    pub captured_output_bytes: usize,
    /// Number of raw output bytes emitted by the child process before any cap.
    pub emitted_byte_count: usize,
    /// Whether the output was truncated by ACI.
    pub was_truncated: bool,
}

// ─── Errors ─────────────────────────────────────────────────────────

/// Errors that can occur during verification.
#[derive(Debug)]
pub enum VerifierError {
    /// The sandbox directory does not exist.
    SandboxNotFound(PathBuf),
    /// The command program was not found in PATH.
    CommandNotFound(String),
    /// Failed to spawn the child process.
    SpawnFailed(std::io::Error),
    /// ACI configuration is invalid.
    InvalidConfig(String),
}

impl fmt::Display for VerifierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SandboxNotFound(p) => write!(f, "sandbox not found: {}", p.display()),
            Self::CommandNotFound(cmd) => write!(f, "command not found: {}", cmd),
            Self::SpawnFailed(e) => write!(f, "spawn failed: {}", e),
            Self::InvalidConfig(msg) => write!(f, "invalid config: {}", msg),
        }
    }
}

impl std::error::Error for VerifierError {}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aci_default_is_valid() {
        let config = AciConfig::default_config();
        assert!(config.validate().is_ok());
        assert_eq!(config.head_lines + config.tail_lines, config.max_lines);
    }

    #[test]
    fn aci_compact_is_valid() {
        let config = AciConfig::compact();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn aci_invalid_config_detected() {
        let config = AciConfig {
            max_lines: 10,
            head_lines: 8,
            tail_lines: 8,
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("exceeds"));
    }

    #[test]
    fn execution_config_defaults() {
        let config = ExecutionConfig::default_config();
        assert_eq!(config.timeout, Duration::from_secs(300));
        assert_eq!(config.max_output_bytes, 10 * 1024 * 1024);
    }

    #[test]
    fn verifier_error_display() {
        let err = VerifierError::CommandNotFound("rustc".into());
        assert_eq!(format!("{}", err), "command not found: rustc");
    }
}
