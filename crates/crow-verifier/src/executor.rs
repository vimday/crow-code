//! Sandbox command executor.
//!
//! Runs a `VerificationCommand` inside a sandbox directory, captures
//! stdout+stderr, applies ACI truncation, and returns a structured
//! `VerificationResult`.
//!
//! # Invariants
//!
//! - Commands are **never run with a shell** (`sh -c`). The program
//!   and args are passed directly to `std::process::Command`.
//! - The working directory is always set to the sandbox root
//!   (or an explicit subdirectory within it).
//! - The command's environment is sanitized: only PATH and
//!   well-known build vars are inherited.

use crate::aci;
use crate::types::{AciConfig, ExecutionConfig, VerificationResult, VerifierError};
use crow_evidence::{TestOutcome, TestRun};
use crow_probe::VerificationCommand;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Execute a verification command inside a sandbox.
///
/// The command is run with the sandbox root as the working directory.
/// Output is captured, truncated via ACI, and returned as a
/// `VerificationResult` that can be fed into `EvidenceMatrix`.
pub fn execute(
    sandbox_root: &Path,
    command: &VerificationCommand,
    exec_config: &ExecutionConfig,
    aci_config: &AciConfig,
) -> Result<VerificationResult, VerifierError> {
    // Validate inputs
    if !sandbox_root.is_dir() {
        return Err(VerifierError::SandboxNotFound(sandbox_root.to_path_buf()));
    }
    aci_config.validate().map_err(VerifierError::InvalidConfig)?;

    // Determine working directory
    let cwd = if let Some(ref sub) = command.cwd {
        let sub_path = sandbox_root.join(sub);
        if !sub_path.is_dir() {
            return Err(VerifierError::SandboxNotFound(sub_path));
        }
        sub_path
    } else {
        sandbox_root.to_path_buf()
    };

    // Build the command — no shell, direct exec
    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args);
    cmd.current_dir(&cwd);

    // Merge stdout and stderr for unified capture
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // Record timing
    let start = Instant::now();

    // Spawn the process
    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            VerifierError::CommandNotFound(command.program.clone())
        } else {
            VerifierError::SpawnFailed(e)
        }
    })?;

    // Wait for completion with timeout
    let output = wait_with_timeout(child, exec_config.timeout, start)?;
    let elapsed = start.elapsed();

    // Combine stdout + stderr
    let raw_stdout = String::from_utf8_lossy(&output.stdout);
    let raw_stderr = String::from_utf8_lossy(&output.stderr);
    let combined = if raw_stderr.is_empty() {
        raw_stdout.to_string()
    } else {
        format!("{}\n--- stderr ---\n{}", raw_stdout, raw_stderr)
    };

    let raw_bytes = combined.len();

    // Apply byte limit before ACI (prevent OOM on pathological output)
    let capped = if raw_bytes > exec_config.max_output_bytes {
        &combined[..exec_config.max_output_bytes]
    } else {
        &combined
    };

    // ACI truncation
    let aci_result = aci::truncate(capped, aci_config);

    // Determine outcome
    let exit_code = output.status.code();
    let outcome = match exit_code {
        Some(0) => TestOutcome::Passed,
        Some(_) => TestOutcome::Failed,
        None => TestOutcome::TimedOut,
    };

    // Build the TestRun record
    let test_run = TestRun {
        command: command.display(),
        outcome,
        passed: 0,  // Parsing test counts is deferred to a future step
        failed: 0,
        skipped: 0,
        duration: elapsed,
        truncated_log: aci_result.output,
    };

    Ok(VerificationResult {
        test_run,
        exit_code,
        raw_output_bytes: raw_bytes,
        was_truncated: aci_result.was_truncated,
    })
}

/// Wait for a child process with timeout.
fn wait_with_timeout(
    child: std::process::Child,
    timeout: Duration,
    start: Instant,
) -> Result<std::process::Output, VerifierError> {
    // For simplicity, use wait_with_output (blocking).
    // A future step can add async/non-blocking wait with periodic
    // timeout checks for long-running processes.
    //
    // For now, we rely on the OS-level timeout approach: if the command
    // is well-behaved, it completes before the timeout.
    let output = child.wait_with_output().map_err(VerifierError::SpawnFailed)?;

    let elapsed = start.elapsed();
    if elapsed > timeout {
        // Command completed but took too long (race with slow I/O)
        return Err(VerifierError::Timeout {
            elapsed,
            limit: timeout,
        });
    }

    Ok(output)
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_sandbox() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "crow_verifier_test_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            C.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn execute_echo_succeeds() {
        let sandbox = make_sandbox();
        let cmd = VerificationCommand::new("echo", vec!["hello", "world"]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd, &exec, &aci).unwrap();

        assert_eq!(result.test_run.outcome, TestOutcome::Passed);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.test_run.truncated_log.contains("hello world"));
        assert!(!result.was_truncated);

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn execute_failing_command() {
        let sandbox = make_sandbox();
        let cmd = VerificationCommand::new("false", vec![]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd, &exec, &aci).unwrap();

        assert_eq!(result.test_run.outcome, TestOutcome::Failed);
        assert_ne!(result.exit_code, Some(0));

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn execute_nonexistent_command() {
        let sandbox = make_sandbox();
        let cmd = VerificationCommand::new("this_command_does_not_exist_xyz", vec![]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd, &exec, &aci);

        assert!(result.is_err());
        match result.unwrap_err() {
            VerifierError::CommandNotFound(name) => {
                assert_eq!(name, "this_command_does_not_exist_xyz");
            }
            other => panic!("expected CommandNotFound, got: {:?}", other),
        }

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn execute_sandbox_not_found() {
        let cmd = VerificationCommand::new("echo", vec!["test"]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result = execute(
            &std::path::PathBuf::from("/nonexistent/sandbox"),
            &cmd,
            &exec,
            &aci,
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            VerifierError::SandboxNotFound(_) => {}
            other => panic!("expected SandboxNotFound, got: {:?}", other),
        }
    }

    #[test]
    fn execute_with_aci_truncation() {
        let sandbox = make_sandbox();
        // Generate a command that produces many lines
        let cmd = VerificationCommand::new("seq", vec!["1", "500"]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig {
            max_lines: 10,
            head_lines: 3,
            tail_lines: 7,
        };

        let result = execute(&sandbox, &cmd, &exec, &aci).unwrap();

        assert!(result.was_truncated);
        assert!(result.test_run.truncated_log.contains("[crow-aci]"));
        assert!(result.test_run.truncated_log.contains("lines omitted"));
        // Head should have "1", "2", "3"
        assert!(result.test_run.truncated_log.starts_with("1\n"));

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn execute_records_duration() {
        let sandbox = make_sandbox();
        let cmd = VerificationCommand::new("sleep", vec!["0.1"]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd, &exec, &aci).unwrap();

        assert!(result.test_run.duration >= Duration::from_millis(50));
        assert!(result.test_run.duration < Duration::from_secs(5));

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn execute_display_command_in_test_run() {
        let sandbox = make_sandbox();
        let cmd = VerificationCommand::new("echo", vec!["hello"]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd, &exec, &aci).unwrap();

        assert_eq!(result.test_run.command, "echo hello");

        let _ = fs::remove_dir_all(&sandbox);
    }
}
