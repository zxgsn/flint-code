//! Tool trait and registry.

use anyhow::Result;
use async_trait::async_trait;
use flint_types::{ToolDefinition, ToolOutput};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Context passed to tool execution.
pub struct ToolContext {
    pub working_dir: PathBuf,
}

/// A tool that the agent can call.
///
/// Implement this trait to add a new tool. The agent loop will call
/// `definition()` to register it and `execute()` when the LLM invokes it.
#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput>;
}

/// Registry of available tools.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        let def = tool.definition();
        self.tools.insert(def.name, Arc::new(tool));
    }

    /// Get tool definitions for the LLM request.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// Execute a tool call.
    pub async fn execute(
        &self,
        name: &str,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(input, ctx).await,
            None => Ok(ToolOutput::error(format!("unknown tool: {}", name))),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
