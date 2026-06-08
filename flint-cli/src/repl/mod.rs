//! REPL loop: input reading, command dispatch, LLM interaction, session management.

pub mod shell;
pub mod slash;

use anyhow::Result;
use flint_agent::{run_turn, Session, ToolContext, ToolRegistry};
use flint_config::Feature;
use flint_mcp::McpManager;
use flint_provider::Provider;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::prompt;

/// Run the interactive REPL loop.
pub async fn run(
    mut config: flint_config::Config,
    mut prov: Box<dyn Provider>,
    mut session: Session,
    registry: &ToolRegistry,
    system: &str,
    ctx: &ToolContext,
    cancel: Arc<AtomicBool>,
    mut mcp_manager: McpManager,
    working_dir: &Path,
) -> Result<()> {
    println!(
        "flint v{} -- {} / {}",
        env!("CARGO_PKG_VERSION"),
        config.provider.r#type,
        config.provider.model
    );
    println!("type /help for commands");

    if config.features.is_enabled(Feature::Skills) {
        let metas = config.load_skill_metas(Some(working_dir));
        if metas.is_empty() {
            println!("Skills: none loaded (add .md files to skill directories)");
        } else {
            println!("Skills: {} available -- /skills to list", metas.len());
        }
    }
    println!();

    let mut current_session_meta: Option<flint_agent::SessionMeta> = None;
    let mut turn_count: u32 = 0;
    let mut total_tool_calls: u32 = 0;

    loop {
        print!("\u{276f} ");
        std::io::stdout().flush()?;

        let input = match crate::input::read_line()? {
            crate::input::InputResult::Line(line) => line,
            crate::input::InputResult::Exit => {
                println!("Bye.");
                break;
            }
        };
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        // !command — shell execution
        if let Some(cmd) = input.strip_prefix('!') {
            shell::execute(cmd, working_dir);
            continue;
        }

        // Slash commands
        if let Some(action) = slash::parse(&input) {
            let mut sc = slash::SlashContext {
                config: &mut config,
                session: &mut session,
                current_session_meta: &mut current_session_meta,
                prov: &mut prov,
                registry,
                ctx,
                cancel: &cancel,
                mcp_manager: &mut mcp_manager,
                working_dir,
                turn_count,
                total_tool_calls,
            };
            let cont = slash::dispatch(action, &mut sc).await?;
            if !cont {
                break;
            }
            // Sync back mutable state
            turn_count = sc.turn_count;
            total_tool_calls = sc.total_tool_calls;
            continue;
        }

        // Normal message -> send to LLM
        turn_count += 1;
        eprintln!("\x1b[34m{}>\x1b[0m {}", turn_count, input);

        let effective_system =
            if let Some(skill) = prompt::match_skill(&input, &config, working_dir) {
                eprintln!("\x1b[90m  skill: {}\x1b[0m", skill.name);
                format!("{}\n\n{}", system, skill.render())
            } else {
                system.to_string()
            };

        session.add_user(&input);
        cancel.store(false, Ordering::Relaxed);
        match run_turn(
            prov.as_ref(),
            &mut session,
            registry,
            &effective_system,
            ctx,
            config.agent.max_turns,
            Some(cancel.clone()),
        )
        .await
        {
            Ok((_text, stats)) => {
                total_tool_calls += stats.tool_calls;
            }
            Err(e) => {
                eprintln!("\n\x1b[31m! Error:\x1b[0m {}", e);
                eprintln!("  Type /setup to reconfigure provider, or /model to switch model.\n");
            }
        }

        // Auto-save session
        if config.session.persistence && !session.is_empty() {
            let session_dir = &config.session.path;
            if !session_dir.exists() {
                let _ = std::fs::create_dir_all(session_dir);
            }

            match &current_session_meta {
                Some(meta) => {
                    let path = session_dir.join(format!("{}.json", meta.id));
                    if let Err(e) = session.update_save(&path, meta) {
                        eprintln!("Warning: Failed to save session: {}", e);
                    }
                }
                None => {
                    let path = session_dir.join(format!("{}.json", uuid::Uuid::new_v4()));
                    if let Err(e) =
                        session.save(&path, &config.provider.r#type, &config.provider.model)
                    {
                        eprintln!("Warning: Failed to save session: {}", e);
                    } else if let Ok((_, meta)) = flint_agent::Session::load(&path) {
                        current_session_meta = Some(meta);
                    }
                }
            }
        }
    }

    mcp_manager.shutdown().await;
    Ok(())
}
