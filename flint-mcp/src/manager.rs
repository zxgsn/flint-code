//! MCP manager — orchestrates multiple MCP server connections.

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

use crate::client::McpClient;
use crate::tool::McpTool;

/// Configuration for a single MCP server.
pub type McpServerConfig = flint_config::McpServerConfig;

/// State of a single MCP server connection.
struct McpServer {
    client: Arc<McpClient>,
    tool_names: Vec<String>, // registered tool names (with prefix)
}

/// Manages connections to multiple MCP servers.
pub struct McpManager {
    servers: HashMap<String, McpServer>,
}

impl McpManager {
    /// Create an empty manager.
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    /// Connect to all configured MCP servers and return their tools.
    ///
    /// Does not register into the ToolRegistry — the caller does that.
    pub async fn connect_all(
        &mut self,
        configs: &HashMap<String, McpServerConfig>,
    ) -> Result<Vec<McpTool>> {
        let mut all_tools = Vec::new();

        for (server_id, config) in configs {
            match self.connect_server(server_id, config).await {
                Ok(tools) => {
                    tracing::info!(
                        "MCP '{}': {} tools discovered",
                        server_id,
                        tools.len()
                    );
                    all_tools.extend(tools);
                }
                Err(e) => {
                    tracing::warn!("MCP '{}' failed to connect: {}", server_id, e);
                    eprintln!("  ⚠ MCP server '{}' failed: {}", server_id, e);
                }
            }
        }

        Ok(all_tools)
    }

    /// Connect to a single MCP server and discover its tools.
    async fn connect_server(
        &mut self,
        server_id: &str,
        config: &McpServerConfig,
    ) -> Result<Vec<McpTool>> {
        let (client, _init) =
            McpClient::spawn(&config.command, &config.args, &config.env).await?;
        let client = Arc::new(client);

        let tool_infos = client.list_tools().await?;

        let tool_names: Vec<String> = tool_infos
            .iter()
            .map(|t| format!("mcp__{}__{}", server_id, t.name))
            .collect();

        let tools: Vec<McpTool> = tool_infos
            .into_iter()
            .map(|info| McpTool {
                server_id: server_id.to_string(),
                info,
                client: Arc::clone(&client),
            })
            .collect();

        self.servers.insert(
            server_id.to_string(),
            McpServer {
                client,
                tool_names,
            },
        );

        Ok(tools)
    }

    /// Reconnect a single server (e.g., after config change).
    /// Returns the new tools, and the old tool names that should be removed.
    pub async fn reload_server(
        &mut self,
        server_id: &str,
        config: &McpServerConfig,
    ) -> Result<(Vec<String>, Vec<McpTool>)> {
        // Collect old tool names for removal
        let old_names = self
            .servers
            .get(server_id)
            .map(|s| s.tool_names.clone())
            .unwrap_or_default();

        // Shutdown old connection
        if let Some(old) = self.servers.remove(server_id) {
            old.client.shutdown().await;
        }

        // Reconnect
        let tools = self.connect_server(server_id, config).await?;

        Ok((old_names, tools))
    }

    /// Get a list of all connected server IDs and their tool counts.
    pub fn status(&self) -> Vec<(&str, usize)> {
        self.servers
            .iter()
            .map(|(id, s)| (id.as_str(), s.tool_names.len()))
            .collect()
    }

    /// Shut down all MCP server connections.
    pub async fn shutdown(&self) {
        for (id, server) in &self.servers {
            tracing::info!("shutting down MCP server '{}'", id);
            server.client.shutdown().await;
        }
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}
