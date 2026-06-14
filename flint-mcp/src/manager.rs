//! MCP manager — orchestrates multiple MCP server connections.
//!
//! Supports two transports:
//! - **stdio**: spawn a local process, communicate via stdin/stdout
//! - **HTTP/SSE**: connect to a remote server via HTTP

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

use crate::client::McpClient;
use crate::http_client::HttpMcpClient;
use crate::tool::McpTool;

pub type McpServerConfig = flint_config::McpServerConfig;

/// Transport-agnostic MCP server handle.
enum McpTransport {
    Stdio {
        client: Arc<McpClient>,
        tool_names: Vec<String>,
    },
    Http {
        _client: Arc<HttpMcpClient>,
        _endpoint: String,
        tool_names: Vec<String>,
    },
}

/// Manages connections to multiple MCP servers (stdio and HTTP).
pub struct McpManager {
    servers: HashMap<String, McpTransport>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    /// Connect to all configured MCP servers and return their tools.
    pub async fn connect_all(
        &mut self,
        configs: &HashMap<String, McpServerConfig>,
    ) -> Result<Vec<McpTool>> {
        let mut all_tools = Vec::new();

        for (server_id, config) in configs {
            match self.connect_server(server_id, config).await {
                Ok(tools) => {
                    tracing::info!("MCP '{}': {} tools discovered", server_id, tools.len());
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

    /// Connect to a single MCP server (auto-detect transport).
    async fn connect_server(
        &mut self,
        server_id: &str,
        config: &McpServerConfig,
    ) -> Result<Vec<McpTool>> {
        if !config.url.is_empty() {
            // HTTP/SSE transport
            self.connect_http(server_id, config).await
        } else if !config.command.is_empty() {
            // stdio transport
            self.connect_stdio(server_id, config).await
        } else {
            anyhow::bail!("MCP server '{}': must specify either 'command' or 'url'", server_id)
        }
    }

    /// Connect via stdio (spawn process).
    async fn connect_stdio(
        &mut self,
        server_id: &str,
        config: &McpServerConfig,
    ) -> Result<Vec<McpTool>> {
        let (client, _init) = McpClient::spawn(&config.command, &config.args, &config.env).await?;
        let client = Arc::new(client);

        let tool_infos = client.list_tools().await?;
        let tool_names: Vec<String> = tool_infos
            .iter()
            .map(|t| format!("mcp__{}__{}", server_id, t.name))
            .collect();

        let tools: Vec<McpTool> = tool_infos
            .into_iter()
            .map(|info| McpTool::new_stdio(server_id, info, Arc::clone(&client)))
            .collect();

        self.servers.insert(
            server_id.to_string(),
            McpTransport::Stdio { client, tool_names },
        );

        Ok(tools)
    }

    /// Connect via HTTP/SSE.
    async fn connect_http(
        &mut self,
        server_id: &str,
        config: &McpServerConfig,
    ) -> Result<Vec<McpTool>> {
        let (client, _init) = HttpMcpClient::connect(&config.url).await?;
        let endpoint = client.get_endpoint().await
            .unwrap_or_else(|| config.url.clone());
        let client = Arc::new(client);

        let tool_infos = client.list_tools(&endpoint).await?;
        let tool_names: Vec<String> = tool_infos
            .iter()
            .map(|t| format!("mcp__{}__{}", server_id, t.name))
            .collect();

        let tools: Vec<McpTool> = tool_infos
            .into_iter()
            .map(|info| McpTool::new_http(server_id, info, Arc::clone(&client), endpoint.clone()))
            .collect();

        self.servers.insert(
            server_id.to_string(),
            McpTransport::Http { _client: client, _endpoint: endpoint, tool_names },
        );

        Ok(tools)
    }

    /// Reconnect a single server. Returns (old_tool_names, new_tools).
    pub async fn reload_server(
        &mut self,
        server_id: &str,
        config: &McpServerConfig,
    ) -> Result<(Vec<String>, Vec<McpTool>)> {
        let old_names = match self.servers.get(server_id) {
            Some(McpTransport::Stdio { tool_names, .. }) => tool_names.clone(),
            Some(McpTransport::Http { tool_names, .. }) => tool_names.clone(),
            None => Vec::new(),
        };

        if let Some(old) = self.servers.remove(server_id) {
            match old {
                McpTransport::Stdio { client, .. } => client.shutdown().await,
                McpTransport::Http { .. } => {} // HTTP connections don't need explicit shutdown
            }
        }

        let tools = self.connect_server(server_id, config).await?;
        Ok((old_names, tools))
    }

    /// Get connected server IDs and tool counts.
    pub fn status(&self) -> Vec<(&str, usize)> {
        self.servers
            .iter()
            .map(|(id, transport)| {
                let count = match transport {
                    McpTransport::Stdio { tool_names, .. } => tool_names.len(),
                    McpTransport::Http { tool_names, .. } => tool_names.len(),
                };
                (id.as_str(), count)
            })
            .collect()
    }

    /// Shut down all connections.
    pub async fn shutdown(&self) {
        for (id, transport) in &self.servers {
            tracing::info!("shutting down MCP server '{}'", id);
            match transport {
                McpTransport::Stdio { client, .. } => client.shutdown().await,
                McpTransport::Http { .. } => {}
            }
        }
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}
