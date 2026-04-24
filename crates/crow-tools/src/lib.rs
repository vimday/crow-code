//! Tool Registry and Permission Enforcement.
//!
//! This crate provides the unified execution layer for all Agent tools (bash, files,
//! grep, glob, subagents, MCP). It enforces permissions via `PermissionEnforcer`
//! before any action reaches the workspace verifier.
//!
//! ## Architecture
//!
//! All tools implement the `Tool` trait which provides:
//! - `name()` — unique identifier
//! - `description()` — LLM-facing documentation
//! - `parameters()` — JSON Schema for function calling
//! - `execute()` — async execution with workspace context
//!
//! The `ToolRegistry` manages tool registration and provides OpenAI-compatible
//! function definitions for the native tool-calling protocol.

pub mod background;
pub mod bash;
pub mod diff_utils;
pub mod file_edit;
pub mod file_state;
pub mod file_write;
pub mod glob;
pub mod grep;
pub mod permission;
pub mod recon;
pub mod subagent;

pub use background::BackgroundProcessManager;
pub use file_state::FileStateStore;
pub use permission::{PermissionEnforcer, WriteMode};

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Context provided to every tool during execution.
pub struct ToolContext<'a> {
    pub workspace_root: &'a Path,
    pub permissions: &'a PermissionEnforcer,
    /// Optional file state tracker for staleness detection.
    /// When set, tools will record file reads and check for external modifications
    /// before edits/writes.
    pub file_state: Option<Arc<FileStateStore>>,
    /// Optional background process manager for async bash commands.
    pub background_manager: Option<Arc<BackgroundProcessManager>>,
    /// Optional delegator for spawning subagents.
    pub subagent_delegator: Option<Arc<dyn SubagentDelegator>>,
}

/// Interface for delegating tasks to a subagent from within a tool.
#[async_trait::async_trait]
pub trait SubagentDelegator: Send + Sync {
    async fn delegate(
        &self,
        task: String,
        role: String,
        focus_paths: Vec<String>,
    ) -> Result<String>;
}

/// Structured output from a tool execution.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn success(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true }
    }
}

/// Base trait for all executable tools.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;

    /// JSON Schema for the tool's input parameters (OpenAI function calling format).
    fn parameters(&self) -> serde_json::Value;

    /// Whether this tool only reads state and never mutates the workspace.
    /// Read-only tools can execute in parallel; write tools acquire exclusive access.
    /// Inspired by Codex's `ToolCallRuntime` RwLock parallelism pattern.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Per-tool timeout. Override for tools that need longer (e.g. bash) or shorter windows.
    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(120)
    }

    /// Execute the tool given a set of JSON parameters and execution context.
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput>;
}

/// A dynamic registry of available tools.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub async fn execute(&self, name: &str, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        let tool = self.tools.get(name).ok_or_else(|| anyhow::anyhow!("Tool not found: {name}"))?;
        tool.execute(args, ctx).await
    }

    /// Return OpenAI-compatible tool definitions for all registered tools.
    /// Sorted by name for deterministic output.
    pub fn tool_definitions(&self) -> Vec<serde_json::Value> {
        let mut defs: Vec<_> = self.tools.values().map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.parameters(),
                    "strict": false
                }
            })
        }).collect();
        defs.sort_by(|a, b| {
            let name_a = a["function"]["name"].as_str().unwrap_or("");
            let name_b = b["function"]["name"].as_str().unwrap_or("");
            name_a.cmp(name_b)
        });
        defs
    }

    /// List all registered tool names (sorted).
    pub fn list(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        names.sort();
        names
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Check if a tool is read-only (safe for parallel execution).
    /// Returns false for unknown tools (conservative default).
    pub fn is_read_only(&self, name: &str) -> bool {
        self.tools.get(name).is_some_and(|t| t.is_read_only())
    }

    /// Get the per-tool timeout duration.
    /// Returns the default 120s for unknown tools.
    pub fn tool_timeout(&self, name: &str) -> std::time::Duration {
        self.tools
            .get(name)
            .map_or(std::time::Duration::from_secs(120), |t| t.timeout())
    }
}
