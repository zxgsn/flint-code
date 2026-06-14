//! Slash command trait and registry.
//!
//! Each slash command implements [`SlashCommand`] and registers itself
//! with [`CommandRegistry`]. This replaces the monolithic `dispatch()` match.

use anyhow::Result;
use async_trait::async_trait;
use flint_agent::{CheckpointStore, Session, SessionMeta, ToolContext, ToolRegistry};
use flint_mcp::McpManager;
use flint_provider::Provider;
use flint_swarm::SwarmManager;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex};

/// Result returned by a command's `execute()` method.
pub enum CommandResult {
    /// Continue the REPL loop.
    Continue,
    /// Exit the REPL loop.
    Quit,
}

/// Trait implemented by all slash commands.
///
/// Each command owns its logic in a separate module. The trait provides
/// a uniform interface for the registry to discover, describe, and execute commands.
#[async_trait]
pub trait SlashCommand: Send + Sync {
    /// Canonical command name (without leading `/`), e.g. `"compact"`.
    fn name(&self) -> &str;

    /// Alternate names that also map to this command.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Short description shown in `/help`.
    fn help(&self) -> &str {
        ""
    }

    /// Whether this command requires an LLM call (used for dependency injection).
    fn needs_llm(&self) -> bool {
        false
    }

    /// Execute the command. Return [`CommandResult`].
    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult>;
}

/// Mutable state needed by slash command handlers.
///
/// All fields are public for direct access by command implementations.
pub struct SlashContext<'a> {
    pub config: &'a mut flint_config::Config,
    pub session: &'a mut Session,
    pub current_session_meta: &'a mut Option<SessionMeta>,
    pub prov: &'a mut Arc<dyn Provider>,
    pub registry: &'a mut ToolRegistry,
    pub ctx: &'a ToolContext,
    pub _cancel: &'a Arc<AtomicBool>,
    pub mcp_manager: &'a mut McpManager,
    pub working_dir: &'a Path,
    pub memory: &'a mut Option<Arc<Mutex<flint_memory::MemoryManager>>>,
    pub swarm: &'a mut Option<Arc<Mutex<SwarmManager>>>,
    pub auto_poke: &'a mut Option<crate::repl::auto_poke::AutoPoke>,
    pub checkpoint_store: CheckpointStore,
    pub _turn_counter: Arc<AtomicU32>,
    /// Argument passed to commands that accept sub-arguments (e.g. /model <name>).
    pub arg: Option<String>,
    pub system: &'a str,
    pub turn_count: u32,
    pub total_tool_calls: u32,
}

/// Registry that maps command names (and aliases) to implementations.
pub struct CommandRegistry {
    /// Primary name → command pointer.
    commands: std::collections::HashMap<&'static str, &'static dyn SlashCommand>,
    /// Alias → canonical name.
    alias_map: std::collections::HashMap<&'static str, &'static str>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: std::collections::HashMap::new(),
            alias_map: std::collections::HashMap::new(),
        }
    }

    /// Register a command. Must be called with a `'static` reference
    /// (typically a `static` or `const` command instance).
    pub fn register(&mut self, cmd: &'static dyn SlashCommand) {
        // Register canonical name
        self.commands.insert(cmd.name(), cmd);
        // Register aliases
        for alias in cmd.aliases() {
            self.alias_map.insert(*alias, cmd.name());
        }
    }

    /// Resolve a parsed command string to its canonical name.
    /// Returns `None` if the command is unknown.
    pub fn resolve<'a>(&self, input: &'a str) -> Option<&'a str> {
        if self.commands.contains_key(input) {
            return Some(input);
        }
        self.alias_map.get(input).copied()
    }

    /// Execute a command by its canonical name.
    pub async fn execute(
        &self,
        name: &str,
        ctx: &mut SlashContext<'_>,
    ) -> Result<CommandResult> {
        let cmd = self
            .commands
            .get(name)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Unknown command: /{}", name))?;
        cmd.execute(ctx).await
    }

    /// List all known command names for `/help`.
    pub fn list_commands(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.commands.keys().copied().collect();
        names.sort();
        names
    }
}
