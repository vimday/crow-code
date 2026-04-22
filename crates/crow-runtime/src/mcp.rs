//! MCP runtime manager for the autonomous agent.

use anyhow::Result;
use crow_mcp::McpClient;
use std::collections::HashMap;
use std::sync::Arc;

pub struct McpManager {
    clients: HashMap<String, Arc<McpClient>>,
    prompt_context: String,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub command: String,
    pub args: Vec<String>,
}

impl McpManager {
    /// Boot configured MCP servers and build the prompt injection material.
    pub async fn boot(
        config_servers: &HashMap<String, ServerConfig>,
    ) -> Result<Self> {
        let mut clients = HashMap::new();
        let mut lines = Vec::new();

        if config_servers.is_empty() {
            return Ok(Self {
                clients,
                prompt_context: String::new(),
            });
        }

        lines.push("=== EXTERNAL MCP TOOLS ===".to_string());
        lines.push("The following read-only external tools are available via ReconAction::McpCall. Use them to fetch contextual data beyond the local workspace limit.".to_string());

        for (name, cfg) in config_servers {
            let args_refs: Vec<&str> = cfg.args.iter().map(std::string::String::as_str).collect();
            let client = match McpClient::spawn(&cfg.command, &args_refs) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("  ⚠️  Failed to spawn MCP server '{name}': {e}");
                    continue;
                }
            };

            // Handshake
            let init = match client.initialize().await {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("  ⚠️  Failed to initialize MCP server '{name}': {e}");
                    continue;
                }
            };
            lines.push(format!(
                "\nServer [{}]: {} v{}",
                name, init.server_info.name, init.server_info.version
            ));

            // Fetch tools
            let tools_res = match client.list_tools().await {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("  ⚠️  Failed to list tools for MCP server '{name}': {e}");
                    continue;
                }
            };
            for tool in tools_res.tools {
                lines.push(format!("  - Tool: {}", tool.name));
                if let Some(desc) = &tool.description {
                    lines.push(format!("    Description: {desc}"));
                }
                lines.push(format!(
                    "    InputSchema: {}",
                    serde_json::to_string(&tool.input_schema).unwrap_or_default()
                ));
            }

            clients.insert(name.clone(), Arc::new(client));
        }

        Ok(Self {
            clients,
            prompt_context: lines.join("\n"),
        })
    }

    /// Retrieve the generated context block to append to the system prompt.
    pub fn prompt_context(&self) -> &str {
        &self.prompt_context
    }

    /// Make a call to a specific tool on a specific server.
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args: Option<serde_json::Value>,
    ) -> Result<crow_mcp::types::CallToolResult> {
        let client = self
            .clients
            .get(server)
            .ok_or_else(|| anyhow::anyhow!("Unknown MCP server: {server}"))?;
        client.call_tool(tool, args).await
    }
}
