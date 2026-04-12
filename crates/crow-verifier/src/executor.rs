//! Workspace isolated command executor.
//!
//! Runs a `VerificationCommand` inside a designated workspace directory, captures
//! stdout+stderr, applies ACI truncation, and returns a structured
//! `VerificationResult`.
//!
//! # Invariants
//!
//! - Commands are **never run with a shell** (`sh -c`). The program
//!   and args are passed directly to `std::process::Command`.
//! - The working directory is boundary-checked to ensure it never
//!   escapes the sandbox root.
//! - The command's environment is sanitized: only explicitly
//!   allowlisted variables (e.g., PATH, HOME) are inherited.
//! - Output is stream-captured with a hard byte limit to prevent OOM
//!   and panics on UTF-8 boundaries.
//! - Execution is strictly bounded by a wall-clock timeout.
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
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

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

/// Execute a verification command inside an isolated workspace context.
///
/// The command is run with the provided root as the working directory.
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
    aci_config
        .validate()
        .map_err(VerifierError::InvalidConfig)?;

    // Determine working directory with strict boundary checks (P1 fix)
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

    // Build the command — no shell, direct exec
    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args);
    cmd.current_dir(&cwd);

    // Give the command its own process group (P1 fix), so we can kill
    // grandchildren processes upon timeout without killing the agent.
    #[cfg(unix)]
    cmd.process_group(0);

    // Sanitize the environment (P1 fix)
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

    // Capture output
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let start = Instant::now();

    // Spawn the process
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            VerifierError::CommandNotFound(command.program.clone())
        } else {
            VerifierError::SpawnFailed(e)
        }
    })?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let unified_buf = Arc::new(Mutex::new(Vec::new()));

    let max_bytes = exec_config.max_output_bytes;
    let remaining = Arc::new(AtomicUsize::new(max_bytes));
    let emitted = Arc::new(AtomicUsize::new(0));

    // Use threads to safely stream output without blocking the child (P2 fix)
    // Both streams share a single atomic byte budget (P1 fix), and a
    // single unified buffer to faithfully interleave stream output (P2 fix).
    let out_clone = Arc::clone(&unified_buf);
    let rem_clone1 = Arc::clone(&remaining);
    let em_clone1 = Arc::clone(&emitted);
    let out_thread = thread::spawn(move || {
        stream_capture(stdout, out_clone, rem_clone1, em_clone1);
    });

    let err_clone = Arc::clone(&unified_buf);
    let rem_clone2 = Arc::clone(&remaining);
    let em_clone2 = Arc::clone(&emitted);
    let err_thread = thread::spawn(move || {
        stream_capture(stderr, err_clone, rem_clone2, em_clone2);
    });

    // Enforce timeout using try_wait (P1 fix)
    let mut exit_code = None;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {
                if start.elapsed() >= exec_config.timeout {
                    kill_process_tree(&mut child);
                    let _ = child.wait(); // Reap the zombie
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                kill_process_tree(&mut child);
                return Err(VerifierError::SpawnFailed(e));
            }
        }
    }
    let elapsed = start.elapsed();

    // The stream threads will exit automatically either because the process
    // exited gracefully or because we killed it (closing its end of the pipes).
    let _ = out_thread.join();
    let _ = err_thread.join();

    let unified_vec = unified_buf.lock().unwrap().clone();
    let raw_bytes = unified_vec.len();

    // Safe UTF-8 decoding after the byte limit
    let combined = String::from_utf8_lossy(&unified_vec).to_string();

    // ACI truncation on the safely decoded string
    let aci_result = aci::truncate(&combined, aci_config);

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
        emitted_byte_count: emitted.load(Ordering::Relaxed),
        was_truncated: aci_result.was_truncated,
    })
}

/// Kill the child process and all its subprocesses if possible.
fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        unsafe {
            // Signal negative PID to kill the whole process group
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        // Fallback to single process kill on non-Unix
        let _ = child.kill();
    }
}

/// Helper to read a stream into a buffer until EOF, capping the buffer size.
/// Uses an atomic budget shared between stdout and stderr.
fn stream_capture(
    mut stream: impl Read,
    buf: Arc<Mutex<Vec<u8>>>,
    remaining: Arc<AtomicUsize>,
    emitted: Arc<AtomicUsize>,
) {
    let mut chunk = vec![0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => {
                emitted.fetch_add(n, Ordering::Relaxed);

                // Atomically claim up to `n` bytes from the remaining budget
                let mut current_rem = remaining.load(Ordering::Relaxed);
                let mut claimed = 0;
                while current_rem > 0 {
                    let to_claim = std::cmp::min(n, current_rem);
                    match remaining.compare_exchange_weak(
                        current_rem,
                        current_rem - to_claim,
                        Ordering::SeqCst,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            claimed = to_claim;
                            break;
                        }
                        Err(x) => current_rem = x,
                    }
                }

                if claimed > 0 {
                    let mut b = buf.lock().unwrap();
                    b.extend_from_slice(&chunk[..claimed]);
                }
                // Keep looping to ensure pipe is drained
            }
            Err(_) => break,
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
    fn execute_with_timeout_kills_child() {
        let sandbox = make_sandbox();
        // A command that sleeps longer than our timeout
        let cmd = VerificationCommand::new("sleep", vec!["10"]);

        let exec = ExecutionConfig {
            timeout: Duration::from_millis(200), // very short timeout
            max_output_bytes: 1024,
        };
        let aci = AciConfig::default_config();

        let start = Instant::now();
        let result = execute(&sandbox, &cmd, &exec, &aci).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(result.test_run.outcome, TestOutcome::TimedOut);

        // Assert we blocked for roughly the timeout, not 10 seconds
        assert!(elapsed >= Duration::from_millis(200));
        assert!(elapsed < Duration::from_secs(2));

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn execute_streaming_output_cap() {
        let sandbox = make_sandbox();

        // Produce a burst of output and then exit
        let shell_cmd = "for i in $(seq 1 1000); do echo \"this is output line $i\"; done";
        let cmd = VerificationCommand::new("sh", vec!["-c", shell_cmd]);

        let exec = ExecutionConfig {
            timeout: Duration::from_secs(5),
            max_output_bytes: 100, // strict byte cap
        };
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd, &exec, &aci).unwrap();

        let log = result.test_run.truncated_log;

        // The raw captured bytes should be strictly <= 100 before utf8 decode
        assert!(result.captured_output_bytes <= 100);
        assert!(result.emitted_byte_count > 100);
        // It shouldn't contain the later lines because of the byte crop
        assert!(!log.contains("line 999"));

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn execute_env_sanitization() {
        // Run env and check its output. Only allowlisted vars should be there.
        let sandbox = make_sandbox();
        let cmd = VerificationCommand::new("env", vec![]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        // Inject a secret into our own environment
        std::env::set_var("CROW_SUPER_SECRET", "this_should_never_leak");

        let result = execute(&sandbox, &cmd, &exec, &aci).unwrap();

        let log = result.test_run.truncated_log;
        assert!(!log.contains("CROW_SUPER_SECRET="));
        assert!(!log.contains("this_should_never_leak"));

        // Ensure some basics like PATH are kept
        assert!(log.contains("PATH="));

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn execute_cwd_bounds_enforcement() {
        let sandbox = make_sandbox();

        // Attempt to supply an absolute path cwd
        let cmd_abs = VerificationCommand {
            program: "pwd".to_string(),
            args: vec![],
            cwd: Some("/etc".to_string()),
        };

        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result = execute(&sandbox, &cmd_abs, &exec, &aci);
        assert!(
            matches!(result, Err(VerifierError::SandboxNotFound(_))),
            "should reject absolute cwd"
        );

        // Attempt traversal escape
        let cmd_esc = VerificationCommand {
            program: "pwd".to_string(),
            args: vec![],
            cwd: Some("../../etc".to_string()),
        };
        let result_esc = execute(&sandbox, &cmd_esc, &exec, &aci);
        assert!(
            matches!(result_esc, Err(VerifierError::SandboxNotFound(_))),
            "should reject traversal bounds escape"
        );

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn execute_uses_isolated_target_dir_per_sandbox() {
        let sandbox_a = make_sandbox();
        let sandbox_b = make_sandbox();
        let cmd = VerificationCommand::new("env", vec![]);
        let exec = ExecutionConfig::default_config();
        let aci = AciConfig::default_config();

        let result_a = execute(&sandbox_a, &cmd, &exec, &aci).unwrap();
        let result_b = execute(&sandbox_b, &cmd, &exec, &aci).unwrap();

        let extract_target = |log: &str| {
            log.lines()
                .find(|line| line.starts_with("CARGO_TARGET_DIR="))
                .map(str::to_string)
                .expect("expected CARGO_TARGET_DIR in env output")
        };

        let target_a = extract_target(&result_a.test_run.truncated_log);
        let target_b = extract_target(&result_b.test_run.truncated_log);

        assert_ne!(target_a, target_b);
        assert!(target_a.contains("crow_target_"));
        assert!(target_b.contains("crow_target_"));

        let _ = fs::remove_dir_all(&sandbox_a);
        let _ = fs::remove_dir_all(&sandbox_b);
    }
}
