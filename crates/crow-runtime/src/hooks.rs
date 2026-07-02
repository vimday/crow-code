//! Lifecycle hooks system for Crow Code.
//!
//! Hooks allow external processes to intercept agent lifecycle events
//! (tool use, session start/stop, prompt submission) via a JSON-based
//! command protocol. Configuration is loaded from `{workspace}/.crow/hooks.json`.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

// ── Event types ──────────────────────────────────────────────────────

/// Lifecycle events that can trigger hook execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "PascalCase")]
pub enum HookEvent {
    /// Fires before a tool is executed.
    PreToolUse,
    /// Fires after a tool has finished executing.
    PostToolUse,
    /// Fires when a new agent session begins.
    SessionStart,
    /// Fires when the agent stops (end of session).
    Stop,
    /// Fires when the user submits a new prompt.
    UserPromptSubmit,
}

// ── Handler definition ───────────────────────────────────────────────

/// A handler describes *how* a hook is executed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookHandler {
    /// Execute an external command. The payload is written to stdin as JSON
    /// and the response is read from stdout.
    Command {
        /// The command and its arguments (e.g. `["python3", "hooks/gate.py"]`).
        command: Vec<String>,
        /// Maximum wall-clock seconds before the command is killed.
        #[serde(default = "default_timeout")]
        timeout_sec: u64,
    },
}

fn default_timeout() -> u64 {
    30
}

// ── Hook definition ──────────────────────────────────────────────────

/// A single hook binding an event to a handler with optional filtering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDefinition {
    /// The lifecycle event this hook listens for.
    pub event: HookEvent,
    /// How to handle the event.
    pub handler: HookHandler,
    /// Optional glob/prefix filter. When set, `PreToolUse`/`PostToolUse`
    /// hooks only fire for tools whose name matches this string.
    #[serde(default)]
    pub tool_name_match: Option<String>,
}

// ── Configuration ────────────────────────────────────────────────────

/// Top-level hooks configuration, typically loaded from `.crow/hooks.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HooksConfig {
    pub hooks: Vec<HookDefinition>,
}

impl HooksConfig {
    /// Attempt to load hooks configuration from `{workspace_root}/.crow/hooks.json`.
    ///
    /// Returns `None` if the file does not exist. Logs a warning and returns
    /// `None` on parse errors so that a malformed config never crashes the agent.
    pub fn load(workspace_root: &Path) -> Option<Self> {
        let path = workspace_root.join(".crow").join("hooks.json");
        if !path.exists() {
            return None;
        }
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<Self>(&contents) {
                Ok(config) => {
                    tracing::info!(
                        hooks_count = config.hooks.len(),
                        "Loaded hooks config from {path}",
                        path = path.display()
                    );
                    Some(config)
                }
                Err(err) => {
                    tracing::warn!(
                        "Failed to parse hooks config at {path}: {err}",
                        path = path.display()
                    );
                    None
                }
            },
            Err(err) => {
                tracing::warn!(
                    "Failed to read hooks config at {path}: {err}",
                    path = path.display()
                );
                None
            }
        }
    }
}

// ── Payload & response ───────────────────────────────────────────────

/// The JSON payload written to a hook command's stdin.
#[derive(Debug, Clone, Serialize)]
pub struct HookPayload {
    /// Which event triggered this hook.
    pub event: HookEvent,
    /// The current session identifier.
    pub session_id: String,
    /// Working directory at the time of invocation.
    pub cwd: String,
    /// ISO-8601 timestamp of when the hook was triggered.
    pub triggered_at: String,
    /// Name of the tool (present for `PreToolUse` / `PostToolUse`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Arguments passed to the tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_args: Option<serde_json::Value>,
    /// Output of the tool (present for `PostToolUse`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
}

/// The JSON response read from a hook command's stdout.
#[derive(Debug, Clone, Deserialize)]
pub struct HookResponse {
    /// Whether the agent should continue execution. Defaults to `true`.
    #[serde(default = "default_true")]
    pub continue_execution: bool,
    /// Human-readable reason when `continue_execution` is `false`.
    pub stop_reason: Option<String>,
    /// If `true`, the tool output is suppressed from the conversation.
    #[serde(default)]
    pub suppress_output: bool,
    /// An optional message injected into the system context.
    pub system_message: Option<String>,
}

impl Default for HookResponse {
    fn default() -> Self {
        Self {
            continue_execution: true,
            stop_reason: None,
            suppress_output: false,
            system_message: None,
        }
    }
}

fn default_true() -> bool {
    true
}

// ── Outcome ──────────────────────────────────────────────────────────

/// The resolved outcome after running all matching hooks for an event.
#[derive(Debug, Clone)]
pub enum HookOutcome {
    /// All hooks passed — continue normally.
    Continue,
    /// At least one hook blocked execution.
    Block {
        /// Why the hook blocked.
        reason: String,
    },
    /// At least one hook requested output suppression.
    SuppressOutput,
}

// ── Runner ───────────────────────────────────────────────────────────

/// Executes lifecycle hooks against their configured commands.
pub struct HookRunner {
    config: HooksConfig,
}

impl HookRunner {
    /// Create a new runner from the given configuration.
    pub fn new(config: HooksConfig) -> Self {
        Self { config }
    }

    /// Returns `true` if at least one hook is registered for `event`.
    pub fn has_hooks_for(&self, event: &HookEvent) -> bool {
        self.config.hooks.iter().any(|h| &h.event == event)
    }

    /// Run all hooks matching the given payload and return the combined outcome.
    ///
    /// Hooks are evaluated sequentially. If any hook blocks, execution stops
    /// immediately and `HookOutcome::Block` is returned.
    pub async fn run(&self, payload: HookPayload) -> HookOutcome {
        let matching: Vec<&HookDefinition> = self
            .config
            .hooks
            .iter()
            .filter(|h| h.event == payload.event)
            .filter(|h| matches_tool_name(h, &payload))
            .collect();

        if matching.is_empty() {
            return HookOutcome::Continue;
        }

        let payload_json = match serde_json::to_string(&payload) {
            Ok(json) => json,
            Err(err) => {
                tracing::error!("Failed to serialize hook payload: {err}");
                return HookOutcome::Continue;
            }
        };

        let mut suppress = false;

        for hook in matching {
            match execute_hook(hook, &payload_json).await {
                Ok(response) => {
                    if let Some(ref msg) = response.system_message {
                        tracing::info!("Hook system message: {msg}");
                    }
                    if !response.continue_execution {
                        let reason = response
                            .stop_reason
                            .unwrap_or_else(|| "blocked by hook".to_string());
                        tracing::warn!("Hook blocked execution: {reason}");
                        return HookOutcome::Block { reason };
                    }
                    if response.suppress_output {
                        suppress = true;
                    }
                }
                Err(err) => {
                    // Hook failures are non-fatal — log and continue.
                    tracing::warn!("Hook execution failed: {err}");
                }
            }
        }

        if suppress {
            HookOutcome::SuppressOutput
        } else {
            HookOutcome::Continue
        }
    }
}

// ── Internal helpers ─────────────────────────────────────────────────

/// Check whether a hook's optional `tool_name_match` filter matches the
/// payload's `tool_name`. If the hook has no filter, it matches everything.
fn matches_tool_name(hook: &HookDefinition, payload: &HookPayload) -> bool {
    match (&hook.tool_name_match, &payload.tool_name) {
        (None, _) => true,
        (Some(pattern), Some(name)) => name.contains(pattern.as_str()),
        (Some(_), None) => false,
    }
}

/// Spawn a single hook command, pipe the payload JSON to stdin, and read
/// the response from stdout.
async fn execute_hook(hook: &HookDefinition, payload_json: &str) -> Result<HookResponse> {
    let HookHandler::Command {
        command,
        timeout_sec,
    } = &hook.handler;

    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("Hook command is empty"))?;

    tracing::debug!("Executing hook command: {program} {args:?}");

    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| anyhow::anyhow!("Failed to spawn hook command '{program}': {err}"))?;

    // Write payload to stdin.
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(payload_json.as_bytes()).await?;
        // Drop stdin to signal EOF.
        drop(stdin);
    }

    let timeout = Duration::from_secs(*timeout_sec);
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("Hook command timed out after {timeout_sec}s"))?
        .map_err(|err| anyhow::anyhow!("Hook command failed: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string());
        tracing::warn!("Hook command exited with {code}: {stderr}");
        // Non-zero exit is treated as a pass-through (non-fatal).
        return Ok(HookResponse::default());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        // Empty stdout means "no opinion" — continue.
        return Ok(HookResponse::default());
    }

    serde_json::from_str::<HookResponse>(&stdout)
        .map_err(|err| anyhow::anyhow!("Failed to parse hook response: {err}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_event_serde() {
        let json = serde_json::to_string(&HookEvent::PreToolUse).expect("serialize");
        assert_eq!(json, r#""PreToolUse""#);

        let parsed: HookEvent = serde_json::from_str(r#""PostToolUse""#).expect("deserialize");
        assert_eq!(parsed, HookEvent::PostToolUse);
    }

    #[test]
    fn test_hooks_config_deserialize() {
        let json = r#"{
            "hooks": [
                {
                    "event": "PreToolUse",
                    "handler": { "type": "command", "command": ["echo", "hello"] },
                    "tool_name_match": "shell"
                }
            ]
        }"#;
        let config: HooksConfig = serde_json::from_str(json).expect("parse config");
        assert_eq!(config.hooks.len(), 1);
        assert_eq!(config.hooks[0].event, HookEvent::PreToolUse);
        assert_eq!(
            config.hooks[0].tool_name_match.as_deref(),
            Some("shell")
        );
    }

    #[test]
    fn test_default_hook_response() {
        let resp = HookResponse::default();
        assert!(resp.continue_execution);
        assert!(!resp.suppress_output);
        assert!(resp.stop_reason.is_none());
    }

    #[test]
    fn test_matches_tool_name_no_filter() {
        let hook = HookDefinition {
            event: HookEvent::PreToolUse,
            handler: HookHandler::Command {
                command: vec!["echo".into()],
                timeout_sec: 10,
            },
            tool_name_match: None,
        };
        let payload = HookPayload {
            event: HookEvent::PreToolUse,
            session_id: String::new(),
            cwd: String::new(),
            triggered_at: String::new(),
            tool_name: Some("shell".into()),
            tool_args: None,
            tool_output: None,
        };
        assert!(matches_tool_name(&hook, &payload));
    }

    #[test]
    fn test_matches_tool_name_with_filter() {
        let hook = HookDefinition {
            event: HookEvent::PreToolUse,
            handler: HookHandler::Command {
                command: vec!["echo".into()],
                timeout_sec: 10,
            },
            tool_name_match: Some("shell".into()),
        };

        let matching = HookPayload {
            event: HookEvent::PreToolUse,
            session_id: String::new(),
            cwd: String::new(),
            triggered_at: String::new(),
            tool_name: Some("run_shell".into()),
            tool_args: None,
            tool_output: None,
        };
        assert!(matches_tool_name(&hook, &matching));

        let non_matching = HookPayload {
            event: HookEvent::PreToolUse,
            session_id: String::new(),
            cwd: String::new(),
            triggered_at: String::new(),
            tool_name: Some("read_file".into()),
            tool_args: None,
            tool_output: None,
        };
        assert!(!matches_tool_name(&hook, &non_matching));
    }

    #[test]
    fn test_has_hooks_for() {
        let config = HooksConfig {
            hooks: vec![HookDefinition {
                event: HookEvent::SessionStart,
                handler: HookHandler::Command {
                    command: vec!["echo".into()],
                    timeout_sec: 5,
                },
                tool_name_match: None,
            }],
        };
        let runner = HookRunner::new(config);
        assert!(runner.has_hooks_for(&HookEvent::SessionStart));
        assert!(!runner.has_hooks_for(&HookEvent::Stop));
    }

    #[test]
    fn test_hook_payload_serialization() {
        let payload = HookPayload {
            event: HookEvent::PreToolUse,
            session_id: "test-session".into(),
            cwd: "/tmp".into(),
            triggered_at: "2026-01-01T00:00:00Z".into(),
            tool_name: Some("shell".into()),
            tool_args: Some(serde_json::json!({"cmd": "ls"})),
            tool_output: None,
        };
        let json = serde_json::to_string(&payload).expect("serialize payload");
        assert!(json.contains("PreToolUse"));
        assert!(json.contains("test-session"));
        // tool_output should be omitted
        assert!(!json.contains("tool_output"));
    }
}
