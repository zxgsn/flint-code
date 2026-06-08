//! Slash command definitions, parsing, and dispatch.

use anyhow::Result;
use flint_agent::{run_turn, Session, ToolContext, ToolRegistry};
use flint_mcp::McpManager;
use flint_provider::Provider;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::display;
use crate::provider;

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
        "quit" | "exit" | "q" => SlashAction::Quit,
        other => SlashAction::Unknown(other.to_string()),
    })
}

/// Mutable state needed by slash command handlers.
pub struct SlashContext<'a> {
    pub config: &'a mut flint_config::Config,
    pub session: &'a mut Session,
    pub current_session_meta: &'a mut Option<flint_agent::SessionMeta>,
    pub prov: &'a mut Box<dyn Provider>,
    pub registry: &'a ToolRegistry,
    pub ctx: &'a ToolContext,
    pub cancel: &'a Arc<AtomicBool>,
    pub mcp_manager: &'a mut McpManager,
    pub working_dir: &'a Path,
    pub memory: &'a Option<Arc<Mutex<flint_memory::MemoryManager>>>,
    pub turn_count: u32,
    pub total_tool_calls: u32,
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
            crate::cmd_config(sc.working_dir)?;
            *sc.config = flint_config::load(Some(sc.working_dir))?;
            println!();
        }
        SlashAction::Setup => {
            dispatch_setup(sc)?;
        }
        SlashAction::Model(name) => {
            dispatch_model(name, sc)?;
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
    crate::setup_ui::run(&env_path)?;
    provider::load_env_override(&env_path);
    let p_type = std::env::var("FLINT_PROVIDER")
        .unwrap_or_else(|_| sc.config.provider.r#type.clone());
    let p_model =
        std::env::var("FLINT_MODEL").unwrap_or_else(|_| sc.config.provider.model.clone());
    match provider::build_provider(&p_type, &p_model) {
        Ok(p) => {
            *sc.prov = p;
            sc.config.provider.r#type = p_type;
            sc.config.provider.model = p_model;
            println!("Provider reloaded.\n");
        }
        Err(e) => println!("Setup incomplete: {}\n", e),
    }
    Ok(())
}

/// /model — switch model
fn dispatch_model(name: Option<String>, sc: &mut SlashContext<'_>) -> Result<()> {
    match name {
        Some(m) => match provider::build_provider(&sc.config.provider.r#type, &m) {
            Ok(p) => {
                *sc.prov = p;
                sc.config.provider.model = m.clone();
                println!("Switched to model: {}\n", m);
            }
            Err(e) => println!("Failed to switch model: {}\n", e),
        },
        None => match crate::model_ui::run(&sc.config.provider.r#type, &sc.config.provider.model) {
            Ok(Some((m, is_custom))) => {
                if is_custom {
                    let env_path = sc.working_dir.join(".env");
                    println!("Custom model: {} -- opening provider setup...\n", m);
                    match crate::setup_ui::run(&env_path) {
                        Ok(true) => {
                            provider::load_env_override(&env_path);
                            let p_type = std::env::var("FLINT_PROVIDER")
                                .unwrap_or_else(|_| sc.config.provider.r#type.clone());
                            match provider::build_provider(&p_type, &m) {
                                Ok(p) => {
                                    *sc.prov = p;
                                    sc.config.provider.r#type = p_type;
                                    sc.config.provider.model = m.clone();
                                    println!("Switched to model: {}\n", m);
                                }
                                Err(e) => println!("Failed to switch model: {}\n", e),
                            }
                        }
                        Ok(false) => println!("Setup cancelled. Model not changed.\n"),
                        Err(e) => println!("Setup error: {}\n", e),
                    }
                } else {
                    match provider::build_provider(&sc.config.provider.r#type, &m) {
                        Ok(p) => {
                            *sc.prov = p;
                            sc.config.provider.model = m.clone();
                            println!("Switched to model: {}\n", m);
                        }
                        Err(e) => println!("Failed to switch model: {}\n", e),
                    }
                }
            }
            Ok(None) => println!("Cancelled.\n"),
            Err(e) => println!("Error: {}\n", e),
        },
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
