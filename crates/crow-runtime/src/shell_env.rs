//! Shell environment snapshot captured at session startup.
//!
//! Provides a consistent view of the host environment (shell, PATH, detected
//! toolchains, proxy settings) so that tool execution behaves identically
//! throughout a session regardless of later environment mutations.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Env‑var keys we propagate into every tool execution context.
// ---------------------------------------------------------------------------

/// Environment variable keys that are forwarded to spawned tool processes.
const PROPAGATED_ENV_KEYS: &[&str] = &[
    "TERM",
    "LANG",
    "LC_ALL",
    "EDITOR",
    "VISUAL",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "CONDA_DEFAULT_ENV",
    "VIRTUAL_ENV",
];

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Snapshot of the shell environment captured at session start.
///
/// Created once via [`ShellSnapshot::capture`] and then shared (immutably)
/// across all tool invocations for the lifetime of the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellSnapshot {
    /// User's default shell (e.g. `/bin/zsh`).
    pub shell: String,
    /// Home directory.
    pub home: PathBuf,
    /// `PATH` entries, split and ordered.
    pub path_entries: Vec<PathBuf>,
    /// Detected Python environment (if any).
    pub python_env: Option<PythonEnv>,
    /// Detected Node.js version string (e.g. `v20.11.0`).
    pub node_version: Option<String>,
    /// Detected Rust toolchain string (e.g. `rustc 1.79.0 …`).
    pub rust_toolchain: Option<String>,
    /// Detected Git version string (e.g. `git version 2.44.0`).
    pub git_version: Option<String>,
    /// Selected environment variables to propagate to child processes.
    pub env_vars: HashMap<String, String>,
}

/// Metadata about the active Python environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythonEnv {
    /// Version string reported by `python3 --version`.
    pub version: String,
    /// Kind of Python environment.
    pub env_type: PythonEnvType,
    /// Root path of the virtual/conda environment, if applicable.
    pub env_path: Option<PathBuf>,
}

/// Discriminant for the detected Python environment type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PythonEnvType {
    /// Conda / Mamba managed environment.
    Conda,
    /// Standard `venv` / `virtualenv`.
    Venv,
    /// System‑wide Python installation.
    System,
}

// ---------------------------------------------------------------------------
// Capture implementation
// ---------------------------------------------------------------------------

impl ShellSnapshot {
    /// Capture the current shell environment.
    ///
    /// Runs lightweight detection commands (`python3 --version`, `node --version`,
    /// etc.) concurrently via `tokio::process::Command`.  This should be called
    /// once at session startup.
    pub async fn capture() -> Self {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

        let path_entries: Vec<PathBuf> = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(PathBuf::from)
            .collect();

        // Run detection probes concurrently.
        let (python_env, node_version, rust_toolchain, git_version) = tokio::join!(
            detect_python_env(),
            detect_version("node", "--version"),
            detect_version("rustc", "--version"),
            detect_version("git", "--version"),
        );

        let env_vars: HashMap<String, String> = PROPAGATED_ENV_KEYS
            .iter()
            .filter_map(|&key| std::env::var(key).ok().map(|v| (key.to_string(), v)))
            .collect();

        tracing::debug!(
            shell = %shell,
            home = %home.display(),
            path_count = path_entries.len(),
            python = ?python_env.as_ref().map(|p| &p.version),
            node = ?node_version,
            rust = ?rust_toolchain,
            git = ?git_version,
            env_count = env_vars.len(),
            "shell environment snapshot captured",
        );

        Self {
            shell,
            home,
            path_entries,
            python_env,
            node_version,
            rust_toolchain,
            git_version,
            env_vars,
        }
    }

    /// Format the snapshot as a Markdown section suitable for inclusion in a
    /// system prompt.
    pub fn as_prompt_section(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("## Shell Environment\n\n");
        out.push_str(&format!("- **Shell**: `{}`\n", self.shell));
        out.push_str(&format!("- **Home**: `{}`\n", self.home.display()));
        out.push_str(&format!(
            "- **PATH entries**: {}\n",
            self.path_entries.len()
        ));

        if let Some(ref py) = self.python_env {
            let env_info = match (&py.env_type, &py.env_path) {
                (PythonEnvType::Conda, Some(p)) => format!(" (conda: `{}`)", p.display()),
                (PythonEnvType::Venv, Some(p)) => format!(" (venv: `{}`)", p.display()),
                _ => String::new(),
            };
            out.push_str(&format!("- **Python**: `{}`{env_info}\n", py.version));
        }
        if let Some(ref v) = self.node_version {
            out.push_str(&format!("- **Node.js**: `{v}`\n"));
        }
        if let Some(ref v) = self.rust_toolchain {
            out.push_str(&format!("- **Rust**: `{v}`\n"));
        }
        if let Some(ref v) = self.git_version {
            out.push_str(&format!("- **Git**: `{v}`\n"));
        }

        if !self.env_vars.is_empty() {
            out.push_str("\n**Propagated env vars**: ");
            let keys: Vec<&str> = self.env_vars.keys().map(String::as_str).collect();
            out.push_str(&keys.join(", "));
            out.push('\n');
        }

        out
    }

    /// Return `true` when a virtual Python environment is active.
    pub fn has_virtual_python(&self) -> bool {
        self.python_env
            .as_ref()
            .is_some_and(|p| p.env_type != PythonEnvType::System)
    }
}

// ---------------------------------------------------------------------------
// Detection helpers
// ---------------------------------------------------------------------------

/// Run a command with a single argument and return its trimmed stdout on
/// success.
async fn detect_version(cmd: &str, arg: &str) -> Option<String> {
    let output = tokio::process::Command::new(cmd)
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    } else {
        None
    }
}

/// Detect the active Python environment by inspecting env vars and falling
/// back to invoking `python3 --version`.
async fn detect_python_env() -> Option<PythonEnv> {
    // Conda takes precedence.
    if let Ok(conda_env) = std::env::var("CONDA_DEFAULT_ENV") {
        let version = detect_version("python3", "--version")
            .await
            .unwrap_or_else(|| format!("conda:{conda_env}"));
        let env_path = std::env::var("CONDA_PREFIX").ok().map(PathBuf::from);
        return Some(PythonEnv {
            version,
            env_type: PythonEnvType::Conda,
            env_path,
        });
    }

    // Standard virtualenv / venv.
    if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
        let version = detect_version("python3", "--version")
            .await
            .unwrap_or_else(|| "unknown".to_string());
        return Some(PythonEnv {
            version,
            env_type: PythonEnvType::Venv,
            env_path: Some(PathBuf::from(venv)),
        });
    }

    // Fallback: system Python.
    let version = detect_version("python3", "--version").await?;
    Some(PythonEnv {
        version,
        env_type: PythonEnvType::System,
        env_path: None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capture_returns_a_snapshot() {
        let snap = ShellSnapshot::capture().await;
        // Shell should never be empty — we fall back to /bin/sh.
        assert!(!snap.shell.is_empty());
        // PATH should contain at least one entry on any reasonable system.
        assert!(!snap.path_entries.is_empty());
    }

    #[tokio::test]
    async fn prompt_section_contains_shell() {
        let snap = ShellSnapshot::capture().await;
        let section = snap.as_prompt_section();
        assert!(section.contains("Shell"));
        assert!(section.contains(&snap.shell));
    }

    #[test]
    fn has_virtual_python_detects_venv() {
        let snap = ShellSnapshot {
            shell: "/bin/zsh".into(),
            home: PathBuf::from("/home/user"),
            path_entries: vec![],
            python_env: Some(PythonEnv {
                version: "Python 3.12.0".into(),
                env_type: PythonEnvType::Venv,
                env_path: Some(PathBuf::from("/home/user/.venv")),
            }),
            node_version: None,
            rust_toolchain: None,
            git_version: None,
            env_vars: HashMap::new(),
        };
        assert!(snap.has_virtual_python());
    }

    #[test]
    fn has_virtual_python_false_for_system() {
        let snap = ShellSnapshot {
            shell: "/bin/zsh".into(),
            home: PathBuf::from("/home/user"),
            path_entries: vec![],
            python_env: Some(PythonEnv {
                version: "Python 3.12.0".into(),
                env_type: PythonEnvType::System,
                env_path: None,
            }),
            node_version: None,
            rust_toolchain: None,
            git_version: None,
            env_vars: HashMap::new(),
        };
        assert!(!snap.has_virtual_python());
    }

    #[test]
    fn prompt_section_with_all_tools() {
        let snap = ShellSnapshot {
            shell: "/bin/zsh".into(),
            home: PathBuf::from("/home/user"),
            path_entries: vec![PathBuf::from("/usr/bin")],
            python_env: Some(PythonEnv {
                version: "Python 3.12.0".into(),
                env_type: PythonEnvType::Conda,
                env_path: Some(PathBuf::from("/opt/conda/envs/ml")),
            }),
            node_version: Some("v20.11.0".into()),
            rust_toolchain: Some("rustc 1.79.0".into()),
            git_version: Some("git version 2.44.0".into()),
            env_vars: HashMap::from([("TERM".into(), "xterm-256color".into())]),
        };
        let section = snap.as_prompt_section();
        assert!(section.contains("Python"));
        assert!(section.contains("conda"));
        assert!(section.contains("Node.js"));
        assert!(section.contains("Rust"));
        assert!(section.contains("Git"));
        assert!(section.contains("Propagated env vars"));
    }
}
