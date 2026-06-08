//! Display helpers: help, skills listing, status.

use std::path::Path;

pub fn print_help() {
    println!(
        "\
Built-in commands:
  /config       Open interactive settings panel
  /setup        Configure provider credentials
  /model        Open model selection (interactive)
  /model <name> Switch to a specific model
  /skills       List available skills
  /skill <name> Load and show a specific skill
  /mcp          Show MCP server status and tools
  /resume       List saved sessions (flint + Claude Code)
  /resume <id>  Restore a saved session
  /compact      Compress conversation history
  /clear        Clear conversation history
  /status       Show current config status
  /help         Show this help message
  /quit         Exit the REPL
  !<command>    Run a shell command (e.g. !ls, !git status)

Keyboard shortcuts:
  Ctrl+A/E      Move to beginning/end of line
  Ctrl+U/K      Delete to beginning/end of line
  Ctrl+W        Delete previous word
  Ctrl+L        Clear screen
  Ctrl+Z        Undo
  Ctrl+J        Insert newline (multiline input)
  Up/Down       Navigate command history
  Left/Right    Move cursor
  Tab           Autocomplete slash commands

Anything else is sent to the LLM as a message.

Skills are auto-injected when your message matches a skill name or description.
You can also explicitly request a skill with [use: <skill-name>] in your message."
    );
}

pub fn print_skills(config: &flint_config::Config, working_dir: &Path) {
    let metas = config.load_skill_metas(Some(working_dir));
    if metas.is_empty() {
        println!("No skills found. Add .md files to:");
        for dir in config.skill_dirs(Some(working_dir)) {
            println!("  {}", dir.display());
        }
        println!();
        return;
    }

    let active_names: Vec<&str> = config
        .features
        .skills
        .active
        .iter()
        .map(|s| s.as_str())
        .collect();
    let filter_active = !active_names.is_empty();

    println!(
        "Skills{}:\n",
        if filter_active {
            " (filtered)"
        } else {
            " (all active)"
        }
    );
    for meta in &metas {
        let status = if !filter_active || active_names.contains(&meta.name.as_str()) {
            "+"
        } else {
            "-"
        };
        let desc = if meta.description.is_empty() {
            String::new()
        } else {
            format!(" -- {}", meta.description)
        };
        println!("  {} {}{}", status, meta.name, desc);
    }
    println!();
}

pub fn print_status(
    config: &flint_config::Config,
    working_dir: &Path,
    turns: u32,
    tool_calls: u32,
    messages: usize,
) {
    let features = &config.features;
    let skill_count = if features.skills.enabled {
        config.load_all_skills(Some(working_dir)).len()
    } else {
        0
    };
    let skill_info = if features.skills.enabled {
        format!("{} loaded", skill_count)
    } else {
        "disabled".to_string()
    };

    let mcp_info = if config.mcp_servers.is_empty() {
        "none".to_string()
    } else {
        format!("{} server(s)", config.mcp_servers.len())
    };

    let (y, n) = ("+", "-");

    println!(
        "\
Provider : {} / {}
Session  : {} (persistence: {})
Skills   : {}
MCP      : {}

This session:
  {} turns    {} tool calls    {} messages

Features:
  {} Skills       {} Memory
  {} Compaction   {} Permissions

Skill directories:",
        config.provider.r#type,
        config.provider.model,
        config.session.path.display(),
        if config.session.persistence { "on" } else { "off" },
        skill_info,
        mcp_info,
        turns,
        tool_calls,
        messages,
        if features.skills.enabled { y } else { n },
        if features.memory.enabled { y } else { n },
        if features.compaction.enabled { y } else { n },
        if features.permissions.enabled { y } else { n },
    );
    for dir in config.skill_dirs(Some(working_dir)) {
        let exists = dir.exists();
        let marker = if exists { "+" } else { "-" };
        println!("  {} {}", marker, dir.display());
    }
}

pub fn print_conversation_history(session: &flint_agent::Session) {
    let messages = &session.messages;
    if messages.is_empty() {
        return;
    }

    println!("Conversation history:");
    println!("{}", "-".repeat(60));

    for msg in messages {
        let text = msg.text();
        if !text.is_empty() {
            let role_display = match msg.role {
                flint_types::Role::User => "\x1b[1;32mUser\x1b[0m",
                flint_types::Role::Assistant => "\x1b[1;34mAssistant\x1b[0m",
                flint_types::Role::System => "\x1b[1;33mSystem\x1b[0m",
                flint_types::Role::Tool => "\x1b[1;35mTool\x1b[0m",
            };

            println!("{}:", role_display);
            if msg.role == flint_types::Role::Assistant {
                crate::markdown::print_markdown(&text);
            } else {
                println!("{}", text);
            }
            println!();
        }
    }

    println!("{}", "-".repeat(60));
    println!();
}
