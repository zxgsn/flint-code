//! Adapter that wraps an MCP tool as a flint Tool.

use anyhow::Result;
use async_trait::async_trait;
use flint_agent::{Tool, ToolContext};
use flint_types::{ToolDefinition, ToolOutput};
use std::sync::Arc;

use crate::client::McpClient;
use crate::protocol::ToolInfo;

/// Wraps an MCP server tool as a flint `Tool`.
///
/// When executed, it delegates to the MCP server via `tools/call`.
pub struct McpTool {
    pub server_id: String,
    pub info: ToolInfo,
    pub client: Arc<McpClient>,
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
        match self.client.call_tool(&self.info.name, input).await {
            Ok(result) => {
                // Convert MCP content blocks to text
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
