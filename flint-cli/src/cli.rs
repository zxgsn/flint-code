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
}
