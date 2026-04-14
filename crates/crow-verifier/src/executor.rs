//! Workspace isolated command executor.
//!
//! Runs a `VerificationCommand` inside a designated workspace directory, captures
//! stdout+stderr, applies ACI truncation, and returns a structured
//! `VerificationResult`.
//!
//! # Invariants
//!
//! - Commands are **never run with a shell** (`sh -c`). The program
//!   and args are passed directly to `tokio::process::Command`.
//! - The working directory is boundary-checked to ensure it never
//!   escapes the sandbox root.
//! - The command's environment is sanitized: only explicitly
//!   allowlisted variables (e.g., PATH, HOME) are inherited.
//! - Execution is strictly bounded by an asynchronous wall-clock timeout.
//!
//! **Note on Security:** This module enforces limits around workspace mutation
//! and resource exhaustion, but does NOT employ OS-level virtualization.
//! Malicious code or aggressive network routines are outside its scope.

use crate::aci;
use crate::types::{AciConfig, ExecutionConfig, VerificationResult, VerifierError};
use crow_evidence::{TestOutcome, TestRun};
use crow_probe::VerificationCommand;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::process::Command;
use tokio::time::timeout;

/// Safe environment variables that are allowed to pass through
/// to the sandbox process. All other variables are cleared.
const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "USER",
    "HOME",
    "LANG",
    "LC_ALL",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "CARGO_TERM_COLOR", // For better formatted rust output if captured
];

/// Helper to perform best-effort lexical path normalization.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if matches!(components.last(), Some(std::path::Component::Normal(_))) {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            std::path::Component::CurDir => {}
            _ => components.push(component),
        }
    }
    components.iter().collect()
}

/// Execute a verification command async inside an isolated workspace context.
pub async fn execute(
    sandbox_root: &Path,
    command: &VerificationCommand,
    exec_config: &ExecutionConfig,
    aci_config: &AciConfig,
) -> Result<VerificationResult, VerifierError> {
    // Validate inputs
    if !sandbox_root.is_dir() {
        return Err(VerifierError::SandboxNotFound(sandbox_root.to_path_buf()));
    }
    aci_config
        .validate()
        .map_err(VerifierError::InvalidConfig)?;

    // Determine working directory with strict boundary checks
    let cwd = if let Some(ref sub) = command.cwd {
        let sub_path = Path::new(sub);
        if sub_path.is_absolute() {
            return Err(VerifierError::SandboxNotFound(sub_path.to_path_buf()));
        }

        let target = sandbox_root.join(sub_path);

        let canonical_root = sandbox_root
            .canonicalize()
            .unwrap_or_else(|_| sandbox_root.to_path_buf());

        let canonical_target = if target.exists() {
            target.canonicalize().map_err(|e| {
                VerifierError::CommandNotFound(format!("cwd canonicalize failed: {}", e))
            })?
        } else {
            normalize_path(&target)
        };

        if !canonical_target.starts_with(&canonical_root) {
            return Err(VerifierError::SandboxNotFound(target));
        }

        target
    } else {
        sandbox_root.to_path_buf()
    };

    // Build the command
    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args);
    cmd.current_dir(&cwd);

    // Give the command its own process group, so we can kill
    // grandchildren processes upon timeout without killing the agent.
    #[cfg(unix)]
    cmd.process_group(0);

    // Sanitize the environment
    cmd.env_clear();
    for var in ENV_ALLOWLIST {
        if let Ok(val) = std::env::var(var) {
            cmd.env(var, val);
        }
    }

    // Isolate build output per sandbox to avoid Cargo file-lock
    // contention once multiple verification sandboxes run concurrently.
    let mut hasher = DefaultHasher::new();
    sandbox_root.hash(&mut hasher);
    let sandbox_hash = format!("{:016x}", hasher.finish());
    let isolated_target = std::env::temp_dir().join(format!("crow_target_{}", sandbox_hash));
    let _ = std::fs::create_dir_all(&isolated_target);
    cmd.env("CARGO_TARGET_DIR", &isolated_target);

    let start = Instant::now();

    // Capture stdout and stderr as pipes so we can stream-read with a byte budget.
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // Spawn the process
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            VerifierError::CommandNotFound(command.program.clone())
        } else {
            VerifierError::SpawnFailed(e)
        }
    })?;

    let child_id = child.id();

    // Take ownership of the pipes so child stays alive for reaping.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    // Drain both pipes CONCURRENTLY under a shared byte budget.
    // Using tokio::select! ensures neither pipe can block the other —
    // whichever has data ready first gets read. This prevents the
    // deadlock where stderr fills its kernel buffer while we wait
    // for stdout EOF.
    let budget = exec_config.max_output_bytes;

    let drain_result = timeout(exec_config.timeout, async {
        use tokio::io::AsyncReadExt;

        let mut buf = Vec::with_capacity(std::cmp::min(budget, 256 * 1024));
        let mut hit_cap = false;
        let mut stdout_done = stdout_pipe.is_none();
        let mut stderr_done = stderr_pipe.is_none();

        while !(hit_cap || (stdout_done && stderr_done)) {
            let remaining = budget.saturating_sub(buf.len());
            if remaining == 0 {
                hit_cap = true;
                break;
            }
            let to_read = std::cmp::min(remaining, 8192);

            // Tag each read result so we know which pipe it came from.
            enum Chunk {
                Stdout(std::io::Result<Vec<u8>>),
                Stderr(std::io::Result<Vec<u8>>),
            }

            let chunk = tokio::select! {
                // Both branches allocate a small temp buffer and return
                // the data by value, avoiding cross-borrow issues.
                v = async {
                    let pipe = stdout_pipe.as_mut().unwrap();
                    let mut tmp = vec![0u8; to_read];
                    pipe.read(&mut tmp).await.map(|n| { tmp.truncate(n); tmp })
                }, if !stdout_done => Chunk::Stdout(v),
                v = async {
                    let pipe = stderr_pipe.as_mut().unwrap();
                    let mut tmp = vec![0u8; to_read];
                    pipe.read(&mut tmp).await.map(|n| { tmp.truncate(n); tmp })
                }, if !stderr_done => Chunk::Stderr(v),
            };

            match chunk {
                Chunk::Stdout(Ok(d)) if d.is_empty() => stdout_done = true,
                Chunk::Stdout(Ok(d)) => buf.extend_from_slice(&d),
                Chunk::Stdout(Err(_)) => stdout_done = true,
                Chunk::Stderr(Ok(d)) if d.is_empty() => stderr_done = true,
                Chunk::Stderr(Ok(d)) => buf.extend_from_slice(&d),
                Chunk::Stderr(Err(_)) => stderr_done = true,
            }
        }

        (buf, hit_cap)
    })
    .await;

    // Determine outcome and ALWAYS reap the child to prevent zombies.
    // Because pipes were taken before the timeout future, `child` is
    // still alive here regardless of whether the drain timed out.
    let (exit_code, unified_vec, output_capped) = match drain_result {
        Ok((buf, hit_cap)) => {
            if hit_cap {
                // Child may be blocked writing to full pipe buffers;
                // kill it so wait() returns promptly.
                if let Some(id) = child_id {
                    kill_process_tree(id);
                }
            }
            match child.wait().await {
                Ok(status) => (status.code(), buf, hit_cap),
                Err(e) => return Err(VerifierError::SpawnFailed(e)),
            }
        }
        Err(_) => {
            // Timeout — kill process group and explicitly reap.
            if let Some(id) = child_id {
                kill_process_tree(id);
            }
            let _ = child.wait().await;
            (None, Vec::new(), false)
        }
    };

    let elapsed = start.elapsed();
    let raw_bytes = unified_vec.len();

    let combined_str = String::from_utf8_lossy(&unified_vec).to_string();

    // ACI truncation on the safely decoded string
    let aci_result = aci::truncate(&combined_str, aci_config);

    let outcome = match exit_code {
        Some(0) => TestOutcome::Passed,
        Some(_) => TestOutcome::Failed,
        None => TestOutcome::TimedOut,
    };

    let display_cmd = if let Some(ref c) = command.cwd {
        format!("[cwd={}] {}", c, command.display())
    } else {
        command.display()
    };

    let test_run = TestRun {
        command: display_cmd,
        outcome,
        passed: 0,
        failed: 0,
        skipped: 0,
        duration: elapsed,
        truncated_log: aci_result.output,
    };

    Ok(VerificationResult {
        test_run,
        exit_code,
        captured_output_bytes: raw_bytes,
        emitted_byte_count: raw_bytes,
        was_truncated: aci_result.was_truncated || output_capped,
    })
}

/// Kill the child process and all its subprocesses if possible via pid
fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        unsafe {
            // Signal negative PID to kill the whole process group
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
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
        // pre-canonicalize so boundary checks test against exact path
        path.canonicalize().unwrap()
    }

    #[tokio::test]
    async fn execute_echo_succeeds() {
        let sandbox = make_sandbox();
        let cmd = VerificationCommand::new("echo", vec!["hello", "world"]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd, &exec, &aci).await.unwrap();

        assert_eq!(result.test_run.outcome, TestOutcome::Passed);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.test_run.truncated_log.contains("hello world"));
    }

    #[tokio::test]
    async fn execute_invalid_args_fails() {
        let sandbox = make_sandbox();
        // ls with invalid flag
        let cmd = VerificationCommand::new("ls", vec!["--this-flag-does-not-exist"]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd, &exec, &aci).await.unwrap();

        assert_eq!(result.test_run.outcome, TestOutcome::Failed);
        assert_ne!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn execute_times_out() {
        let sandbox = make_sandbox();
        // sleep 2
        let cmd = VerificationCommand::new("sleep", vec!["2"]);
        let exec = ExecutionConfig {
            timeout: std::time::Duration::from_millis(50), // kill it fast (0.05s)
            ..ExecutionConfig::default_config()
        };
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd, &exec, &aci).await.unwrap();

        assert_eq!(result.test_run.outcome, TestOutcome::TimedOut);
        assert_eq!(result.exit_code, None);
    }
}
