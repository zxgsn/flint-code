//! Adapter that wraps an MCP tool as a flint Tool.
//!
//! Supports both stdio and HTTP/SSE transports.

use anyhow::Result;
use async_trait::async_trait;
use flint_agent::{Tool, ToolContext};
use flint_types::{ToolDefinition, ToolOutput};
use std::sync::Arc;

use crate::client::McpClient;
use crate::http_client::HttpMcpClient;
use crate::protocol::ToolInfo;

/// Transport-specific client reference.
enum TransportClient {
    Stdio(Arc<McpClient>),
    Http(Arc<HttpMcpClient>, String), // client + endpoint
}

/// Wraps an MCP server tool as a flint `Tool`.
pub struct McpTool {
    pub server_id: String,
    pub info: ToolInfo,
    transport: TransportClient,
}

impl McpTool {
    /// Create a tool backed by a stdio MCP client.
    pub fn new_stdio(server_id: &str, info: ToolInfo, client: Arc<McpClient>) -> Self {
        Self {
            server_id: server_id.to_string(),
            info,
            transport: TransportClient::Stdio(client),
        }
    }

    /// Create a tool backed by an HTTP/SSE MCP client.
    pub fn new_http(server_id: &str, info: ToolInfo, client: Arc<HttpMcpClient>, endpoint: String) -> Self {
        Self {
            server_id: server_id.to_string(),
            info,
            transport: TransportClient::Http(client, endpoint),
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: format!("mcp__{}__{}", self.server_id, self.info.name),
            description: format!(
                "[MCP:{}] {}",
                self.server_id,
                if self.info.description.is_empty() {
                    &self.info.name
                } else {
                    &self.info.description
                }
            ),
            parameters: self.info.input_schema.clone(),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let result = match &self.transport {
            TransportClient::Stdio(client) => {
                client.call_tool(&self.info.name, input).await
            }
            TransportClient::Http(client, endpoint) => {
                client.call_tool(endpoint, &self.info.name, input).await
            }
        };

        match result {
            Ok(result) => {
                let text: String = result
                    .content
                    .iter()
                    .map(|block| match block {
                        crate::protocol::ContentBlock::Text { text } => text.clone(),
                        crate::protocol::ContentBlock::Image { mime_type, .. } => {
                            format!("[image: {}]", mime_type)
                        }
                        crate::protocol::ContentBlock::Resource { resource } => {
                            serde_json::to_string_pretty(resource)
                                .unwrap_or_else(|_| "[resource]".to_string())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if result.is_error {
                    Ok(ToolOutput::error(text))
                } else {
                    Ok(ToolOutput::text(text))
                }
            }
            Err(e) => Ok(ToolOutput::error(format!(
                "MCP call failed ({}): {}",
                self.server_id, e
            ))),
        }
    }
}
