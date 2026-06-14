//! Slash command subsystem.
//!
//! Each command is a standalone module implementing [`SlashCommand`].
//! The registry maps command names to their implementations.

pub mod traits;
pub mod quit;
pub mod help;
pub mod clear;
pub mod status;
pub mod skills;
pub mod mcp;
pub mod unknown;
pub mod compact;
pub mod resume;
pub mod config;
pub mod setup;
pub mod model;
pub mod memory;
pub mod swarm;
pub mod poke;
pub mod undo;

pub use traits::*;
pub use quit::QUIT_COMMAND;
pub use help::HELP_COMMAND;
pub use clear::CLEAR_COMMAND;
pub use status::STATUS_COMMAND;
pub use skills::SKILLS_COMMAND;
pub use mcp::MCP_COMMAND;
pub use compact::COMPACT_COMMAND;
pub use resume::RESUME_COMMAND;
pub use config::CONFIG_COMMAND;
pub use setup::SETUP_COMMAND;
pub use model::MODEL_COMMAND;
pub use memory::MEMORY_COMMAND;
pub use swarm::SWARM_COMMAND;
pub use poke::POKE_COMMAND;
pub use undo::UNDO_COMMAND;
pub use unknown::UnknownCommand;

use anyhow::Result;

/// Build the default command registry with all built-in commands.
pub fn build_registry() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    reg.register(&QUIT_COMMAND);
    reg.register(&HELP_COMMAND);
    reg.register(&CLEAR_COMMAND);
    reg.register(&STATUS_COMMAND);
    reg.register(&SKILLS_COMMAND);
    reg.register(&MCP_COMMAND);
    reg.register(&COMPACT_COMMAND);
    reg.register(&RESUME_COMMAND);
    reg.register(&CONFIG_COMMAND);
    reg.register(&SETUP_COMMAND);
    reg.register(&MODEL_COMMAND);
    reg.register(&MEMORY_COMMAND);
    reg.register(&SWARM_COMMAND);
    reg.register(&POKE_COMMAND);
    reg.register(&UNDO_COMMAND);
    reg
}

/// Parsed slash command.
pub enum SlashAction {
    Config,
    Setup,
    Model(Option<String>),
    Skill(Option<String>),
    Clear,
    Compact,
    Help,
    Status,
    Skills,
    Mcp,
    Memory(Option<String>),
    Resume(Option<String>),
    Swarm(Option<String>),
    Poke(Option<String>),
    Undo,
    Quit,
    Unknown(String),
}

/// Parse a slash command from input. Returns None if input doesn't start with '/'.
pub fn parse(input: &str) -> Option<SlashAction> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }
    let cmd = input[1..].split_whitespace().next()?;
    Some(match cmd {
        "config" => SlashAction::Config,
        "setup" => SlashAction::Setup,
        "model" => {
            let arg = input[1..].split_whitespace().nth(1);
            SlashAction::Model(arg.map(|s| s.to_string()))
        }
        "skill" => {
            let arg = input[1..].split_whitespace().nth(1);
            SlashAction::Skill(arg.map(|s| s.to_string()))
        }
        "clear" => SlashAction::Clear,
        "compact" => SlashAction::Compact,
        "help" | "h" | "?" => SlashAction::Help,
        "status" => SlashAction::Status,
        "skills" => SlashAction::Skills,
        "mcp" => SlashAction::Mcp,
        "memory" | "mem" => {
            let arg = input[1..].split_whitespace().nth(1);
            SlashAction::Memory(arg.map(|s| s.to_string()))
        }
        "resume" => {
            let arg = input[1..].split_whitespace().nth(1);
            SlashAction::Resume(arg.map(|s| s.to_string()))
        }
        "swarm" => {
            let arg = input[1..].split_whitespace().nth(1);
            SlashAction::Swarm(arg.map(|s| s.to_string()))
        }
        "poke" => {
            let arg = input[1..].split_whitespace().nth(1);
            SlashAction::Poke(arg.map(|s| s.to_string()))
        }
        "undo" => SlashAction::Undo,
        "quit" | "exit" | "q" => SlashAction::Quit,
        other => SlashAction::Unknown(other.to_string()),
    })
}

/// Execute a slash command. Returns `Ok(true)` to continue the REPL, `Ok(false)` to quit.
pub async fn dispatch(action: SlashAction, sc: &mut SlashContext<'_>) -> Result<bool> {
    match action {
        SlashAction::Quit => return Ok(false),
        SlashAction::Help => {
            HELP_COMMAND.execute(sc).await?;
        }
        SlashAction::Clear => {
            CLEAR_COMMAND.execute(sc).await?;
        }
        SlashAction::Compact => {
            COMPACT_COMMAND.execute(sc).await?;
        }
        SlashAction::Resume(arg) => {
            sc.arg = arg;
            RESUME_COMMAND.execute(sc).await?;
        }
        SlashAction::Config => {
            CONFIG_COMMAND.execute(sc).await?;
        }
        SlashAction::Setup => {
            SETUP_COMMAND.execute(sc).await?;
        }
        SlashAction::Model(name) => {
            sc.arg = name;
            MODEL_COMMAND.execute(sc).await?;
        }
        SlashAction::Skill(name) => {
            // /skill is handled as a sub-command of skills
            match name {
                Some(n) => match sc.config.load_skill_by_name(&n, Some(sc.working_dir)) {
                    Some(skill) => {
                        println!("\n+ Skill: {}", skill.name);
                        if !skill.description.is_empty() {
                            println!("  Description: {}", skill.description);
                        }
                        println!("  Source: {}", skill.source.display());
                        println!("\n---\n{}\n---\n", skill.prompt);
                    }
                    None => {
                        println!("Skill '{}' not found. Use /skills to list available skills.\n", n);
                    }
                },
                None => {
                    SKILLS_COMMAND.execute(sc).await?;
                }
            }
        }
        SlashAction::Status => {
            STATUS_COMMAND.execute(sc).await?;
        }
        SlashAction::Skills => {
            SKILLS_COMMAND.execute(sc).await?;
        }
        SlashAction::Mcp => {
            MCP_COMMAND.execute(sc).await?;
        }
        SlashAction::Memory(sub) => {
            sc.arg = sub;
            MEMORY_COMMAND.execute(sc).await?;
        }
        SlashAction::Swarm(sub) => {
            sc.arg = sub;
            SWARM_COMMAND.execute(sc).await?;
        }
        SlashAction::Poke(sub) => {
            sc.arg = sub;
            POKE_COMMAND.execute(sc).await?;
        }
        SlashAction::Undo => {
            UNDO_COMMAND.execute(sc).await?;
        }
        SlashAction::Unknown(cmd) => {
            let uc = UnknownCommand { cmd };
            uc.execute(sc).await?;
        }
    }
    Ok(true)
}
