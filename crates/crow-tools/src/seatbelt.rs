//! macOS Seatbelt Sandbox Integration.
//!
//! Provides `sandbox-exec` wrapping for tool commands, constraining filesystem
//! access, network usage, and subprocess spawning via Apple's Sandbox Profile
//! Language (SBPL). The module degrades gracefully when `sandbox-exec` is not
//! available (e.g. on Linux or CI environments).
//!
//! ## Design
//!
//! A [`SeatbeltPolicy`] captures the desired access boundaries. Calling
//! [`SeatbeltPolicy::generate_profile`] produces a valid `.sbpl` string that
//! can be passed to `sandbox-exec -p <profile>`. The [`SandboxedCommand`]
//! struct wraps the original command with the appropriate invocation.
//!
//! Workspace root always receives write access. Common system paths
//! (`/usr`, `/bin`, `/Library`, Homebrew prefixes) get read-only access so
//! toolchains resolve correctly.

use std::collections::HashSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};


/// Policy configuration for seatbelt sandboxing.
#[derive(Debug, Clone)]
pub struct SeatbeltPolicy {
    /// Paths with read-only access.
    pub read_paths: HashSet<PathBuf>,
    /// Paths with read-write access.
    pub write_paths: HashSet<PathBuf>,
    /// Whether outbound network access is allowed.
    pub network_allowed: bool,
    /// Whether subprocess spawning (`process-exec`, `process-fork`) is allowed.
    pub allow_subprocess: bool,
    /// Workspace root (always receives write access).
    pub workspace_root: PathBuf,
}

impl SeatbeltPolicy {
    /// Create a new policy scoped to the given workspace.
    ///
    /// Populates sensible defaults for macOS system paths, Homebrew prefixes,
    /// and Rust toolchain directories so that `cargo`, `rustc`, and common
    /// CLI tools work inside the sandbox.
    pub fn for_workspace(workspace_root: PathBuf) -> Self {
        let mut read_paths = HashSet::new();

        // Core system paths required by most CLI tools.
        for p in [
            "/usr",
            "/bin",
            "/sbin",
            "/Library",
            "/System",
            "/private/tmp",
            "/private/var",
            // Homebrew
            "/opt/homebrew",
            "/usr/local",
        ] {
            read_paths.insert(PathBuf::from(p));
        }

        let mut write_paths = HashSet::new();
        write_paths.insert(workspace_root.clone());
        write_paths.insert(PathBuf::from("/private/tmp"));

        // Rust toolchain and build cache directories.
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            read_paths.insert(home.join(".cargo"));
            read_paths.insert(home.join(".rustup"));
            write_paths.insert(home.join(".cache"));
        }

        Self {
            read_paths,
            write_paths,
            network_allowed: true,
            allow_subprocess: true,
            workspace_root,
        }
    }

    /// Generate the Sandbox Profile Language (`.sbpl`) content.
    ///
    /// The profile starts with `(version 1)` and `(deny default)`, then
    /// selectively allows the configured access categories.
    pub fn generate_profile(&self) -> String {
        let mut profile = String::with_capacity(2048);

        // Header — deny everything by default.
        profile.push_str("(version 1)\n(deny default)\n\n");

        // Read-only filesystem access.
        if !self.read_paths.is_empty() {
            profile.push_str("(allow file-read*\n");
            for path in sorted_paths(&self.read_paths) {
                let _ = writeln!(profile, "    (subpath \"{path}\")", path = path.display());
            }
            profile.push_str(")\n\n");
        }

        // Read-write filesystem access.
        if !self.write_paths.is_empty() {
            // Grant both read and write — write without read is rarely useful.
            profile.push_str("(allow file-read* file-write*\n");
            for path in sorted_paths(&self.write_paths) {
                let _ = writeln!(profile, "    (subpath \"{path}\")", path = path.display());
            }
            profile.push_str(")\n\n");
        }

        // Process control.
        if self.allow_subprocess {
            profile.push_str("(allow process-exec* process-fork)\n\n");
        }

        // Network access.
        if self.network_allowed {
            profile.push_str("(allow network*)\n\n");
        }

        // Common macOS sandbox operations required for most processes.
        profile.push_str(
            "(allow sysctl-read)\n\
             (allow mach-lookup)\n\
             (allow signal (target self))\n\
             (allow user-preference-read)\n\
             (allow iokit-open)\n",
        );

        profile
    }

    /// Check whether `sandbox-exec` is available on this system.
    pub fn is_available() -> bool {
        Path::new("/usr/bin/sandbox-exec").exists()
    }

    /// Wrap a command to run under the seatbelt sandbox.
    ///
    /// Returns `None` if `sandbox-exec` is not present on the system,
    /// enabling graceful degradation on non-macOS hosts.
    pub fn wrap_command(&self, cmd: &str) -> Option<SandboxedCommand> {
        if !Self::is_available() {
            tracing::debug!("sandbox-exec not found; skipping seatbelt wrapping");
            return None;
        }
        let profile = self.generate_profile();
        Some(SandboxedCommand {
            profile,
            inner_cmd: cmd.to_string(),
        })
    }

    /// Extend the policy with proxy-aware network socket paths.
    ///
    /// Parses `HTTP_PROXY` and `HTTPS_PROXY` environment variables and, when
    /// they point to Unix domain sockets, adds the socket paths to
    /// `read_paths` so the sandbox permits proxy connections.
    #[must_use]
    pub fn with_proxy_support(mut self) -> Self {
        for var in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            if let Ok(val) = std::env::var(var) {
                // Unix domain socket proxies use a file path.
                if let Some(socket_path) = val.strip_prefix("unix://") {
                    let path = PathBuf::from(socket_path);
                    if let Some(parent) = path.parent() {
                        self.read_paths.insert(parent.to_path_buf());
                    }
                }
            }
        }
        self
    }

    /// Restrict the policy to deny outbound network access.
    #[must_use]
    pub fn without_network(mut self) -> Self {
        self.network_allowed = false;
        self
    }

    /// Restrict the policy to deny subprocess spawning.
    #[must_use]
    pub fn without_subprocess(mut self) -> Self {
        self.allow_subprocess = false;
        self
    }
}

/// A command prepared for execution inside a seatbelt sandbox.
#[derive(Debug, Clone)]
pub struct SandboxedCommand {
    /// The generated SBPL profile content.
    pub profile: String,
    /// The original shell command to run inside the sandbox.
    pub inner_cmd: String,
}

impl SandboxedCommand {
    /// Build the argument vector for invoking `sandbox-exec`.
    ///
    /// The returned `Vec<String>` can be passed directly to
    /// `tokio::process::Command::new(args[0]).args(&args[1..])`.
    pub fn to_command_args(&self) -> Vec<String> {
        vec![
            "/usr/bin/sandbox-exec".to_string(),
            "-p".to_string(),
            self.profile.clone(),
            "bash".to_string(),
            "-c".to_string(),
            self.inner_cmd.clone(),
        ]
    }

    /// Convenience: build a `tokio::process::Command` ready to spawn.
    pub fn to_tokio_command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("/usr/bin/sandbox-exec");
        cmd.args(["-p", &self.profile, "bash", "-c", &self.inner_cmd]);
        cmd
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Return paths sorted for deterministic profile output.
fn sorted_paths(set: &HashSet<PathBuf>) -> Vec<&PathBuf> {
    let mut paths: Vec<&PathBuf> = set.iter().collect();
    paths.sort();
    paths
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn profile_starts_with_version_and_deny() {
        let policy = SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/workspace"));
        let profile = policy.generate_profile();
        assert!(profile.starts_with("(version 1)\n(deny default)\n"));
    }

    #[test]
    fn profile_contains_sbpl_syntax() {
        let policy = SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/workspace"));
        let profile = policy.generate_profile();

        // Must have the core SBPL keywords.
        assert!(profile.contains("(allow file-read*"));
        assert!(profile.contains("(allow file-read* file-write*"));
        assert!(profile.contains("(allow process-exec* process-fork)"));
        assert!(profile.contains("(allow network*)"));
        assert!(profile.contains("(allow sysctl-read)"));
        assert!(profile.contains("(allow mach-lookup)"));
        assert!(profile.contains("(subpath \"/tmp/workspace\")"));
    }

    #[test]
    fn workspace_root_always_in_write_paths() {
        let ws = PathBuf::from("/home/user/project");
        let policy = SeatbeltPolicy::for_workspace(ws.clone());
        assert!(policy.write_paths.contains(&ws));

        let profile = policy.generate_profile();
        assert!(profile.contains("(subpath \"/home/user/project\")"));
    }

    #[test]
    fn no_network_removes_network_allow() {
        let policy =
            SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/ws")).without_network();
        let profile = policy.generate_profile();
        assert!(!profile.contains("(allow network*)"));
    }

    #[test]
    fn no_subprocess_removes_process_allow() {
        let policy =
            SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/ws")).without_subprocess();
        let profile = policy.generate_profile();
        assert!(!profile.contains("(allow process-exec* process-fork)"));
    }

    #[test]
    fn wrap_command_returns_none_when_sandbox_exec_missing() {
        // On CI / Linux, sandbox-exec is not at /usr/bin/sandbox-exec.
        // This test is conditional on the binary's presence.
        if !SeatbeltPolicy::is_available() {
            let policy = SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/ws"));
            assert!(policy.wrap_command("echo hello").is_none());
        }
    }

    #[test]
    fn sandboxed_command_args_structure() {
        let cmd = SandboxedCommand {
            profile: "(version 1)".to_string(),
            inner_cmd: "echo hello".to_string(),
        };
        let args = cmd.to_command_args();
        assert_eq!(args[0], "/usr/bin/sandbox-exec");
        assert_eq!(args[1], "-p");
        assert_eq!(args[2], "(version 1)");
        assert_eq!(args[3], "bash");
        assert_eq!(args[4], "-c");
        assert_eq!(args[5], "echo hello");
    }

    #[test]
    fn proxy_support_adds_socket_paths() {
        // SAFETY: Test runs sequentially; no other thread reads HTTP_PROXY.
        unsafe {
            std::env::set_var("HTTP_PROXY", "unix:///var/run/proxy.sock");
        }

        let policy = SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/ws"))
            .with_proxy_support();

        assert!(policy.read_paths.contains(&PathBuf::from("/var/run")));

        // Clean up.
        unsafe {
            std::env::remove_var("HTTP_PROXY");
        }
    }

    #[test]
    fn proxy_support_ignores_http_urls() {
        // SAFETY: Test runs sequentially; no other thread reads HTTPS_PROXY.
        unsafe {
            std::env::set_var("HTTPS_PROXY", "http://proxy.example.com:8080");
        }

        let before = SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/ws"));
        let before_count = before.read_paths.len();

        let after = SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/ws"))
            .with_proxy_support();
        assert_eq!(after.read_paths.len(), before_count);

        unsafe {
            std::env::remove_var("HTTPS_PROXY");
        }
    }

    #[test]
    fn default_policy_includes_system_paths() {
        let policy = SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/ws"));
        assert!(policy.read_paths.contains(&PathBuf::from("/usr")));
        assert!(policy.read_paths.contains(&PathBuf::from("/bin")));
        assert!(policy.read_paths.contains(&PathBuf::from("/opt/homebrew")));
        assert!(policy.read_paths.contains(&PathBuf::from("/System")));
    }

    #[test]
    fn profile_deterministic_output() {
        let policy = SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/ws"));
        let p1 = policy.generate_profile();
        let p2 = policy.generate_profile();
        assert_eq!(p1, p2, "profile generation must be deterministic");
    }

    #[test]
    fn for_workspace_includes_home_dirs_when_home_set() {
        if let Ok(home) = std::env::var("HOME") {
            let policy = SeatbeltPolicy::for_workspace(PathBuf::from("/tmp/ws"));
            let home = PathBuf::from(home);
            assert!(policy.read_paths.contains(&home.join(".cargo")));
            assert!(policy.read_paths.contains(&home.join(".rustup")));
            assert!(policy.write_paths.contains(&home.join(".cache")));
        }
    }
}
