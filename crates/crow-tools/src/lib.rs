//! Tool Registry and Permission Enforcement.
//!
//! This crate provides the unified execution layer for all Agent tools (bash, files,
//! subagents, MCP). It enforces permissions via `PermissionEnforcer` before any action
//! reaches the workspace verifier.

pub mod bash;
pub mod file_edit;
pub mod file_write;
pub mod permission;
pub mod recon;

pub use permission::{PermissionEnforcer, WriteMode};

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

/// Context provided to every tool during execution.
pub struct ToolContext<'a> {
    pub frozen_root: &'a Path,
    pub permissions: &'a PermissionEnforcer,
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
    pub fn tool_definitions(&self) -> Vec<serde_json::Value> {
        self.tools.values().map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.parameters(),
                    "strict": false
                }
            })
        }).collect()
    }

    /// List all registered tool names.
    pub fn list(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}
