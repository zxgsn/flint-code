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
use flint_agent::{run_turn, Session, ToolContext, ToolRegistry};
use flint_mcp::McpManager;
use flint_provider::Provider;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::display;
use crate::provider;

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
        SlashAction::Help => display::print_help(),
        SlashAction::Clear => {
            *sc.session = Session::new();
            *sc.current_session_meta = None;
            println!("Session cleared.\n");
        }
        SlashAction::Compact => {
            dispatch_compact(sc).await?;
        }
        SlashAction::Resume(arg) => {
            dispatch_resume(arg, sc).await?;
        }
        SlashAction::Config => {
            CONFIG_COMMAND.execute(sc).await?;
        }
        SlashAction::Setup => {
            dispatch_setup(sc)?;
        }
        SlashAction::Model(name) => {
            sc.arg = name;
            MODEL_COMMAND.execute(sc).await?;
        }
        SlashAction::Status => {
            display::print_status(
                sc.config,
                sc.working_dir,
                sc.turn_count,
                sc.total_tool_calls,
                sc.session.messages.len(),
            );
            println!();
        }
        SlashAction::Skills => {
            display::print_skills(sc.config, sc.working_dir);
        }
        SlashAction::Mcp => {
            let status = sc.mcp_manager.status();
            if status.is_empty() {
                println!("No MCP servers configured.");
                println!("Add [mcp_servers.<id>] to .flint.toml:\n");
                println!("  [mcp_servers.memory]");
                println!("  command = \"npx\"");
                println!("  args = [\"-y\", \"@modelcontextprotocol/server-memory\"]\n");
            } else {
                println!("MCP Servers:");
                for (id, count) in &status {
                    println!("  + {} ({} tools)", id, count);
                }
                println!();
            }
        }
        SlashAction::Skill(name) => match name {
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
                display::print_skills(sc.config, sc.working_dir);
            }
        },
        SlashAction::Memory(sub) => {
            dispatch_memory(sub, sc);
        }
        SlashAction::Swarm(sub) => {
            dispatch_swarm(sub, sc);
        }
        SlashAction::Poke(sub) => {
            dispatch_poke(sub, sc);
        }
        SlashAction::Undo => {
            dispatch_undo(sc);
        }
        SlashAction::Unknown(cmd) => {
            println!(
                "Unknown command: /{}\nType /help for available commands.\n",
                cmd
            );
        }
    }
    Ok(true)
}

/// /compact — summarize conversation history
async fn dispatch_compact(sc: &mut SlashContext<'_>) -> Result<()> {
    if sc.session.is_empty() {
        println!("Nothing to compact.\n");
        return Ok(());
    }
    let msg_count = sc.session.messages.len();
    eprintln!("Compacting {} messages...", msg_count);

    let mut history = String::new();
    for msg in &sc.session.messages {
        let role = match msg.role {
            flint_types::Role::User => "User",
            flint_types::Role::Assistant => "Assistant",
            flint_types::Role::System => "System",
            flint_types::Role::Tool => "Tool",
        };
        let text = msg.text();
        if !text.is_empty() {
            history.push_str(&format!("{}: {}\n\n", role, text));
        }
    }

    let compact_prompt = format!(
        "Summarize the following conversation concisely. Keep all key facts, decisions, file paths, and code context. Output only the summary, no preamble.\n\n{}",
        history
    );

    let mut compact_session = Session::new();
    compact_session.add_user(&compact_prompt);
    match run_turn(
        sc.prov.as_ref(),
        &mut compact_session,
        sc.registry,
        "You are a summarizer. Be concise.",
        sc.ctx,
        5,
        None,
        65536,
        true, // silent
        None,
        None,
    )
    .await
    {
        Ok((summary, _)) => {
            if summary.is_empty() {
                println!("Compaction failed: empty summary.\n");
                return Ok(());
            }
            let keep = 4usize.min(msg_count);
            let tail: Vec<flint_types::Message> =
                sc.session.messages[msg_count - keep..].to_vec();
            *sc.session = Session::new();
            sc.session
                .messages
                .push(flint_types::Message::system(&format!(
                    "[Compacted context from {} messages]\n\n{}",
                    msg_count, summary
                )));
            sc.session.messages.extend(tail);
            println!(
                "Compacted {} -> {} messages.\n",
                msg_count,
                sc.session.messages.len()
            );
        }
        Err(e) => {
            println!("Compaction failed: {}\n", e);
        }
    }
    Ok(())
}

/// /resume — restore a saved session
async fn dispatch_resume(arg: Option<String>, sc: &mut SlashContext<'_>) -> Result<()> {
    match arg {
        Some(id) => {
            let session_dir = &sc.config.session.path;
            let flint_sessions =
                flint_agent::Session::list_sessions(session_dir).unwrap_or_default();
            let found_flint = flint_sessions.iter().find(|s| s.id.starts_with(&id));

            let claude_sessions =
                crate::session_import::list_claude_sessions(sc.working_dir).unwrap_or_default();
            let found_claude = claude_sessions.iter().find(|(_, m)| m.id.starts_with(&id));

            if let Some(meta) = found_flint {
                let path = session_dir.join(format!("{}.json", meta.id));
                match flint_agent::Session::load(&path) {
                    Ok((loaded_session, loaded_meta)) => {
                        *sc.session = loaded_session;
                        *sc.current_session_meta = Some(loaded_meta.clone());
                        println!(
                            "Resumed session: {} ({})",
                            loaded_meta.title,
                            &loaded_meta.id[..8]
                        );
                        println!(
                            "  Provider: {} / {}",
                            loaded_meta.provider, loaded_meta.model
                        );
                        println!("  Messages: {}\n", loaded_meta.message_count);
                    }
                    Err(e) => println!("Error loading session: {}\n", e),
                }
            } else if let Some((path, _meta)) = found_claude {
                match crate::session_import::import_session(path) {
                    Ok((loaded_session, loaded_meta)) => {
                        *sc.session = loaded_session;
                        *sc.current_session_meta = Some(loaded_meta.clone());
                        let id_display = &loaded_meta.id[..8.min(loaded_meta.id.len())];
                        println!(
                            "Resumed Claude Code session: {} ({})",
                            loaded_meta.title, id_display
                        );
                        println!(
                            "  Provider: {} / {}",
                            loaded_meta.provider, loaded_meta.model
                        );
                        println!("  Messages: {}\n", loaded_meta.message_count);
                    }
                    Err(e) => println!("Error loading Claude Code session: {}\n", e),
                }
            } else {
                println!("Session not found: {}\n", id);
            }
        }
        None => match crate::resume_ui::run(sc.config, sc.working_dir) {
            Ok(Some((path, meta))) => {
                let (loaded_session, loaded_meta) = if meta.provider == "claude-code" {
                    crate::session_import::import_session(&path)?
                } else {
                    flint_agent::Session::load(&path)?
                };
                *sc.session = loaded_session;
                *sc.current_session_meta = Some(loaded_meta.clone());
                let id_display = &loaded_meta.id[..8.min(loaded_meta.id.len())];
                let prefix = if meta.provider == "claude-code" {
                    "Resumed Claude Code session"
                } else {
                    "Resumed session"
                };
                println!("{}: {} ({})", prefix, loaded_meta.title, id_display);
                println!(
                    "  Provider: {} / {}",
                    loaded_meta.provider, loaded_meta.model
                );
                println!("  Messages: {}\n", loaded_meta.message_count);
                display::print_conversation_history(sc.session);
            }
            Ok(None) => println!("Cancelled.\n"),
            Err(e) => println!("Error: {}\n", e),
        },
    }
    Ok(())
}

/// /setup — configure provider
fn dispatch_setup(sc: &mut SlashContext<'_>) -> Result<()> {
    let env_path = sc.working_dir.join(".env");
    crate::setup_ui::run_edit(&env_path)?;
    provider::load_env_override(&env_path);
    let p_type = std::env::var("FLINT_PROVIDER")
        .unwrap_or_else(|_| sc.config.provider.r#type.clone());
    let p_model =
        std::env::var("FLINT_MODEL").unwrap_or_else(|_| sc.config.provider.model.clone());
    match provider::build_provider_with_config(&p_type, &p_model, &sc.config.provider.model_base_urls, &sc.config.provider.model_api_keys) {
        Ok(p) => {
            *sc.prov = Arc::from(p);
            sc.config.provider.r#type = p_type;
            sc.config.provider.model = p_model;
            println!("Provider reloaded.\n");
        }
        Err(e) => println!("Setup incomplete: {}\n", e),
    }
    Ok(())
}

/// /memory — show memory status, list, search, or edit core blocks
fn dispatch_memory(sub: Option<String>, sc: &mut SlashContext<'_>) {
    let mem = match sc.memory {
        Some(m) => m,
        None => {
            println!("Memory is disabled. Enable it in config: [features.memory] enabled = true\n");
            return;
        }
    };

    match sub.as_deref() {
        Some("list") | Some("ls") => {
            let mm = mem.lock().unwrap();
            let entries = mm.list(None);
            if entries.is_empty() {
                println!("No memories stored.\n");
                return;
            }
            println!("{} memories:\n", entries.len());
            for entry in &entries {
                println!(
                    "  [{}][{}] {} (id: {}, accessed: {}x)",
                    entry.category, entry.scope, entry.content, entry.id, entry.access_count
                );
            }
            println!();
        }
        Some("core") => {
            let mm = mem.lock().unwrap();
            let blocks = mm.core_blocks();
            if blocks.is_empty() {
                println!("No core memory blocks.\n");
                return;
            }
            println!("Core Memory Blocks:\n");
            for block in blocks {
                let ro = if block.read_only { " (read-only)" } else { "" };
                println!("  [{}]{} (limit: {} chars)", block.label, ro, block.limit);
                println!("  {}\n", block.content);
            }
        }
        Some("help") => {
            println!(
                "\
Memory commands:
  /memory          Show memory status
  /memory list     List all stored memories
  /memory core     Show core memory blocks
  /memory help     Show this help

Memory tools (available to the agent):
  memory_remember    Save a fact/preference/correction
  memory_forget      Remove a memory by ID
  memory_search      Search memories by keyword
  memory_list        List all memories
  memory_update_core Update a core memory block\n"
            );
        }
        _ => {
            // Default: show memory status
            let mm = mem.lock().unwrap();
            let (core, project, global) = mm.counts();
            println!(
                "\
Memory Status:
  Core blocks: {}
  Project memories: {}
  Global memories: {}
  Total: {}

Use /memory list to see all memories.
Use /memory core to see core blocks.
Use /memory help for all commands.\n",
                core,
                project,
                global,
                core + project + global
            );
        }
    }
}

/// /swarm — show swarm status
fn dispatch_swarm(sub: Option<String>, sc: &mut SlashContext<'_>) {
    let swarm = match sc.swarm {
        Some(s) => s,
        None => {
            println!("Swarm is disabled. Enable it in config: [features.swarm] enabled = true\n");
            return;
        }
    };

    match sub.as_deref() {
        Some(s) if s.starts_with("spawn") => {
            // /swarm spawn <prompt> — directly spawn a terminal sub-agent for testing
            let prompt = s.strip_prefix("spawn").unwrap_or("").trim();
            let prompt = if prompt.is_empty() {
                "Hello from the coordinator! Please introduce yourself and confirm you are running in a new terminal."
                    .to_string()
            } else {
                prompt.to_string()
            };
            let mut sm = swarm.lock().unwrap();
            match sm.spawn_terminal(prompt, None, false, None) {
                Ok(result) => {
                    println!(
                        "Spawned terminal agent [{}] (task {})\n\
                         A new terminal window should appear.\n\
                         Use /swarm status to check progress.\n",
                        &result.agent_id[result.agent_id.len()-4..],
                        result.task_id,
                    );
                }
                Err(e) => {
                    println!("Spawn failed: {}\n", e);
                }
            }
        }
        Some("status") | Some("st") => {
            let sm = swarm.lock().unwrap();
            let agents = sm.agent_status();
            let tasks = sm.task_status();
            println!(
                "Swarm: {} active agents, {} tasks\n",
                sm.active_agent_count(),
                tasks.len()
            );
            if !agents.is_empty() {
                println!("Agents:");
                for (id, status, task_id) in &agents {
                    let task_info = task_id
                        .as_ref()
                        .map(|t| format!(" -> {}", t))
                        .unwrap_or_default();
                    println!("  {} [{}]{}", id, status, task_info);
                }
                println!();
            }
            if !tasks.is_empty() {
                println!("Tasks:");
                for task in &tasks {
                    println!("  {} [{}]: {}", task.id, task.status, task.content);
                }
                println!();
            }
        }
        Some("tasks") => {
            let sm = swarm.lock().unwrap();
            let tasks = sm.task_status();
            if tasks.is_empty() {
                println!("No tasks.\n");
            } else {
                println!("{} tasks:\n", tasks.len());
                for task in &tasks {
                    let result = task
                        .result
                        .as_ref()
                        .map(|r| {
                            let preview = if r.len() > 80 {
                                format!("{}...", &r[..80])
                            } else {
                                r.clone()
                            };
                            format!(" -> {}", preview)
                        })
                        .unwrap_or_default();
                    println!("  {} [{}]{}: {}", task.id, task.status, result, task.content);
                }
                println!();
            }
        }
        Some("viewer") | Some("view") => {
            flint_swarm::log::open_viewer();
            println!(
                "Opened viewer ({})\nLogs: {}\n",
                flint_swarm::log::viewer_mode_name(),
                flint_swarm::log::log_dir().display()
            );
        }
        Some("help") => {
            println!(
                "\
Swarm commands:
  /swarm              Show swarm status
  /swarm spawn [task] Spawn terminal sub-agent (for testing)
  /swarm status       Show agents and tasks
  /swarm tasks        List all tasks
  /swarm viewer       Open log viewer window
  /swarm help         Show this help

Swarm tools (available to the agent):
  swarm spawn    Spawn sub-agent (in-process, interactive, or terminal)
  swarm status   Check agent and task status
  swarm stop     Stop an agent or all agents
  swarm list     List all tasks
  swarm viewer   Open a terminal window tailing sub-agent logs
  swarm clean    Delete log files

Logs are saved to ~/.flint/swarm-logs/\n"
            );
        }
        _ => {
            // Default: show status
            let sm = swarm.lock().unwrap();
            let tasks = sm.task_status();
            println!(
                "Swarm Status:\n  Active agents: {}\n  Total tasks: {}\n\n\
                 Use /swarm status for details.\n\
                 Use /swarm help for all commands.\n",
                sm.active_agent_count(),
                tasks.len()
            );
        }
    }
}

fn dispatch_poke(sub: Option<String>, sc: &mut SlashContext<'_>) {
    let ap = match sc.auto_poke {
        Some(ref mut ap) => ap,
        None => {
            println!("Auto-poke is not available (todo tool not registered).\n");
            return;
        }
    };

    match sub.as_deref() {
        Some("on") | Some("enable") => {
            ap.enabled = true;
            ap.consecutive_pokes = 0;
            println!("Auto-poke: enabled (max {} consecutive pokes)\n", ap.max_pokes);
        }
        Some("off") | Some("disable") => {
            ap.enabled = false;
            println!("Auto-poke: disabled\n");
        }
        Some("status") => {
            let incomplete = flint_agent::todo::incomplete_count(&ap.store);
            println!(
                "Auto-poke: {} | Pokes this round: {}/{} | Incomplete todos: {}\n",
                if ap.enabled { "enabled" } else { "disabled" },
                ap.consecutive_pokes,
                ap.max_pokes,
                incomplete,
            );
        }
        Some("help") => {
            println!(
                "\
Auto-poke automatically sends a \"continue working\" message when
incomplete todos remain after a turn completes.

Commands:
  /poke on       Enable auto-poke
  /poke off      Disable auto-poke
  /poke status   Show current state
  /poke help     Show this help

Safety: stops after {} consecutive pokes without user input,
and stops immediately on non-retryable errors (auth, billing, etc.).\n",
                ap.max_pokes
            );
        }
        _ => {
            // Toggle
            ap.enabled = !ap.enabled;
            if ap.enabled {
                ap.consecutive_pokes = 0;
                println!("Auto-poke: enabled\n");
            } else {
                println!("Auto-poke: disabled\n");
            }
        }
    }
}

fn dispatch_undo(sc: &mut SlashContext<'_>) {
    crate::repl::perform_undo(
        &sc.checkpoint_store,
        sc.session,
        sc.working_dir,
    );
}
