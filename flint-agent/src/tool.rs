//! Tool trait and registry.

use anyhow::Result;
use async_trait::async_trait;
use flint_types::{ToolDefinition, ToolOutput};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Context passed to tool execution.
#[derive(Clone)]
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

    /// Custom timeout for this tool. Override for long-running tools like swarm spawn.
    /// Returns None to use the default timeout (120s).
    fn timeout(&self) -> Option<Duration> {
        None
    }
}

/// Registry of available tools.
///
/// The internal map is wrapped in `Arc` so registries can be cheaply cloned.
/// Cloned registries share the same tool instances — registering a tool on one
/// clone makes it visible to all others.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<HashMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Arc::new(HashMap::new()),
        }
    }

    /// Register a tool. Uses `Arc::make_mut` — if this registry is a clone
    /// sharing the map with others, the map is copied-on-write.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        let def = tool.definition();
        Arc::make_mut(&mut self.tools).insert(def.name, Arc::new(tool));
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

    /// Get the custom timeout for a tool, or None for default.
    pub fn tool_timeout(&self, name: &str) -> Option<Duration> {
        self.tools.get(name).and_then(|t| t.timeout())
    }

    /// Remove a tool by name. Returns true if it existed.
    pub fn remove(&mut self, name: &str) -> bool {
        Arc::make_mut(&mut self.tools).remove(name).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// List all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
