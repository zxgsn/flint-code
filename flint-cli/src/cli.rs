//! CLI argument definitions.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "flint", about = "Easy-to-hack agent harness in Rust")]
pub struct Cli {
    /// Initial prompt (runs once and exits, or starts REPL if omitted)
    pub prompt: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Working directory
    #[arg(global = true, long, default_value = ".")]
    pub dir: String,

    /// System prompt override (inline string)
    #[arg(global = true, long)]
    pub system: Option<String>,

    /// System prompt from file (reads file content as system prompt)
    #[arg(global = true, long)]
    pub system_file: Option<String>,

    /// Initial message to inject into session before REPL starts.
    /// The agent processes this message immediately, then enters interactive mode.
    #[arg(global = true, long)]
    pub initial_message: Option<String>,

    /// Initial message from file (reads file content as the initial message).
    /// Useful for swarm sub-agents to avoid shell escaping issues.
    #[arg(global = true, long)]
    pub initial_message_file: Option<String>,

    /// Message file for inter-agent communication.
    /// The REPL checks this file for pending messages from the coordinator
    /// before each turn. Messages are consumed (file is cleared) after reading.
    #[arg(global = true, long)]
    pub message_file: Option<String>,

    /// Router address for real-time agent communication (127.0.0.1:port).
    /// When set, the agent connects to the MessageRouter for instant message delivery.
    #[arg(global = true, long)]
    pub router_addr: Option<String>,

    /// Agent ID (used by sub-agents to identify themselves to the router).
    #[arg(global = true, long)]
    pub agent_id: Option<String>,

    /// Display client mode: connect to a swarm agent via router and act as
    /// a thin terminal client (show agent output, forward user input).
    /// Requires --router-addr and --agent-id.
    #[arg(global = true, long)]
    pub display: bool,

    /// Path to a SpawnContext JSON file. When set, the agent loads context
    /// from this file and runs as a sub-agent with inherited context.
    #[arg(global = true, long)]
    pub spawn_context: Option<String>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Open interactive settings panel
    Config,
    /// Configure provider (interactive if no args, or automated with args)
    Setup(SetupArgs),
    /// Run the agent (default when no subcommand is given)
    Agent(AgentArgs),
}

#[derive(Parser)]
pub struct SetupArgs {
    /// Provider name: openai | anthropic
    #[arg(long)]
    pub provider: Option<String>,
    /// API key
    #[arg(long)]
    pub key: Option<String>,
    /// Base URL (for OpenAI-compatible endpoints)
    #[arg(long)]
    pub base_url: Option<String>,
    /// Model name
    #[arg(long)]
    pub model: Option<String>,
}

#[derive(Parser)]
pub struct AgentArgs {
    /// Initial prompt (skips REPL, runs once and exits)
    pub prompt: Option<String>,

    /// LLM provider: anthropic | openai
    #[arg(long)]
    pub provider: Option<String>,

    /// Model name
    #[arg(long)]
    pub model: Option<String>,

    /// System prompt
    #[arg(long)]
    pub system: Option<String>,

    /// Initial message to inject into session before REPL starts.
    #[arg(long)]
    pub initial_message: Option<String>,

    /// Message file for inter-agent communication.
    #[arg(long)]
    pub message_file: Option<String>,

    /// Router address for real-time agent communication.
    #[arg(long)]
    pub router_addr: Option<String>,

    /// Agent ID (used by sub-agents to identify themselves to the router).
    #[arg(long)]
    pub agent_id: Option<String>,

    /// Path to SpawnContext JSON (loaded by sub-agents spawned in new terminals).
    #[arg(long)]
    pub spawn_context: Option<String>,
}
