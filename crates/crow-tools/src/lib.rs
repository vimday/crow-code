//! Tool Registry and Permission Enforcement.
//!
//! This crate provides the unified execution layer for all Agent tools (bash, files,
//! subagents, MCP). It enforces permissions via `PermissionEnforcer` before any action
//! reaches the workspace verifier.

pub mod permission;

pub use permission::{PermissionEnforcer, WriteMode};

use anyhow::Result;
use std::collections::HashMap;

/// Base trait for all executable tools.
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    
    /// Execute the tool given a set of string parameters.
    fn execute(&self, args: &HashMap<String, String>) -> Result<String>;
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

    pub fn execute(&self, name: &str, args: &HashMap<String, String>) -> Result<String> {
        let tool = self.tools.get(name).ok_or_else(|| anyhow::anyhow!("Tool not found: {name}"))?;
        tool.execute(args)
    }
}
