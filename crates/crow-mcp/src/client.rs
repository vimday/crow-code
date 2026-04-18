//! High-level Model Context Protocol (MCP) Client.
//!
//! Exposes a type-safe RPC API over `StdioTransport` specifically for the MCP lifecycle.

use crate::transport::StdioTransport;
use crate::types::{
    CallToolRequest, CallToolResult, ClientCapabilities, Implementation, InitializeParams,
    InitializeResult, ListToolsResult,
};
use anyhow::{Context, Result};
use std::sync::Arc;

pub struct McpClient {
    transport: Arc<StdioTransport>,
}

impl McpClient {
    /// Spawn an MCP server child process and wrap it in a client.
    pub fn spawn(cmd: &str, args: &[&str]) -> Result<Self> {
        let transport = StdioTransport::spawn(cmd, args)?;
        Ok(Self {
            transport: Arc::new(transport),
        })
    }

    /// Perform the `initialize` handshake.
    pub async fn initialize(&self) -> Result<InitializeResult> {
        let params = InitializeParams {
            protocol_version: "2024-11-05".into(), // Or the latest protocol string
            capabilities: ClientCapabilities {
                roots: None,
                sampling: None,
            },
            client_info: Implementation {
                name: "crow-agent".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        };

        let params_json = serde_json::to_value(&params)?;

        let response = self
            .transport
            .send_request("initialize", Some(params_json))
            .await?;

        let result = response
            .result
            .context("Missing result in initialize response")?;

        let init_result: InitializeResult = serde_json::from_value(result)?;

        // Send the initialized notification
        self.transport
            .send_notification("notifications/initialized", None)
            .await?;

        Ok(init_result)
    }

    /// Ask the server for the list of available tools.
    pub async fn list_tools(&self) -> Result<ListToolsResult> {
        let response = self
            .transport
            .send_request("tools/list", None) // Sometimes requires pagination args
            .await?;

        let result = response
            .result
            .context("Missing result in tools/list response")?;

        let tools_result: ListToolsResult = serde_json::from_value(result)?;
        Ok(tools_result)
    }

    /// Call a specific tool by name with arguments.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<CallToolResult> {
        let params = CallToolRequest {
            name: name.into(),
            arguments,
        };

        let params_json = serde_json::to_value(&params)?;

        let response = self
            .transport
            .send_request("tools/call", Some(params_json))
            .await?;

        let result = response
            .result
            .context("Missing result in tools/call response")?;

        let call_result: CallToolResult = serde_json::from_value(result)?;
        Ok(call_result)
    }
}
