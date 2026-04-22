//! Tool Registry and Permission Enforcement.
//!
//! This crate provides the unified execution layer for all Agent tools (bash, files,
//! subagents, MCP). It enforces permissions via `PermissionEnforcer` before any action
//! reaches the workspace verifier.

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

/// Base trait for all executable tools.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    
    /// Execute the tool given a set of JSON parameters and execution context.
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<String>;
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

    pub async fn execute(&self, name: &str, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<String> {
        let tool = self.tools.get(name).ok_or_else(|| anyhow::anyhow!("Tool not found: {name}"))?;
        tool.execute(args, ctx).await
    }
}
