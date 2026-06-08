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
use std::sync::{Arc, Mutex};

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
    memory: Option<Arc<Mutex<flint_memory::MemoryManager>>>,
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

    if let Some(ref mem) = memory {
        let mm = mem.lock().unwrap();
        let (core, project, global) = mm.counts();
        println!(
            "Memory: {} core blocks, {} project, {} global -- /memory to manage",
            core, project, global
        );
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
                memory: &memory,
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

        let mut effective_system =
            if let Some(skill) = prompt::match_skill(&input, &config, working_dir) {
                eprintln!("\x1b[90m  skill: {}\x1b[0m", skill.name);
                format!("{}\n\n{}", system, skill.render())
            } else {
                system.to_string()
            };

        // Per-turn memory: search archival memory for relevant context
        if let Some(ref mem) = memory {
            let mut mm = mem.lock().unwrap();
            if let Some(relevant) = mm.format_relevant_memories(&input) {
                effective_system.push_str(&format!("\n\n{}", relevant));
            }
        }

        session.add_user(&input);
        cancel.store(false, Ordering::Relaxed);

        // Remember the last assistant message index for extraction
        let pre_turn_msg_count = session.messages.len();

        match run_turn(
            prov.as_ref(),
            &mut session,
            registry,
            &effective_system,
            ctx,
            config.agent.max_turns,
            Some(cancel.clone()),
            config.agent.max_output_chars,
        )
        .await
        {
            Ok((_text, stats)) => {
                total_tool_calls += stats.tool_calls;

                // Auto-extract memories from the conversation (if enabled)
                if config.features.memory.auto_extract {
                    if let Some(ref mem) = memory {
                        auto_extract_memories(
                            mem,
                            &session,
                            pre_turn_msg_count,
                            &mut prov,
                            registry,
                            ctx,
                        )
                        .await;
                    }
                }
            }
            Err(e) => {
                eprintln!("\n\x1b[31m! Error:\x1b[0m {}", e);
                eprintln!("  Type /setup to reconfigure provider, or /model to switch model.\n");
            }
        }

        // Auto-compaction: if session exceeds 80% of context window, compact automatically
        if config.features.is_enabled(Feature::Compaction) {
            let total_chars: usize = session.messages.iter().map(|m| m.text().len()).sum();
            let threshold = (config.agent.context_window_chars as f64 * 0.8) as usize;
            if total_chars > threshold && session.messages.len() > 6 {
                eprintln!(
                    "\x1b[90m  auto-compact: {} chars exceeds {} threshold\x1b[0m",
                    total_chars, threshold
                );
                dispatch_auto_compact(
                    &mut session,
                    &mut current_session_meta,
                    &mut prov,
                    registry,
                    ctx,
                )
                .await;
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

/// Auto-compaction: summarize session and replace with compact form.
async fn dispatch_auto_compact(
    session: &mut Session,
    current_session_meta: &mut Option<flint_agent::SessionMeta>,
    prov: &mut Box<dyn Provider>,
    registry: &ToolRegistry,
    ctx: &ToolContext,
) {
    let msg_count = session.messages.len();

    let mut history = String::new();
    for msg in &session.messages {
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
        prov.as_ref(),
        &mut compact_session,
        registry,
        "You are a summarizer. Be concise.",
        ctx,
        5,
        None,
        65536,
    )
    .await
    {
        Ok((summary, _)) => {
            if summary.is_empty() {
                return;
            }
            let keep = 4usize.min(msg_count);
            let tail: Vec<flint_types::Message> =
                session.messages[msg_count - keep..].to_vec();
            *session = Session::new();
            session
                .messages
                .push(flint_types::Message::system(&format!(
                    "[Auto-compacted from {} messages]\n\n{}",
                    msg_count, summary
                )));
            session.messages.extend(tail);
            eprintln!(
                "\x1b[90m  auto-compact: {} → {} messages\x1b[0m",
                msg_count,
                session.messages.len()
            );
            // Update session meta if available
            if let Some(ref mut meta) = current_session_meta {
                meta.message_count = session.messages.len();
            }
        }
        Err(e) => {
            eprintln!("\x1b[90m  auto-compact failed: {}\x1b[0m", e);
        }
    }
}

/// Extract memories from the latest conversation turn using the LLM.
async fn auto_extract_memories(
    memory: &Arc<Mutex<flint_memory::MemoryManager>>,
    session: &Session,
    pre_turn_msg_count: usize,
    prov: &mut Box<dyn Provider>,
    registry: &ToolRegistry,
    ctx: &ToolContext,
) {
    // Find the last user message and assistant response
    let messages = &session.messages;
    let mut last_user = None;
    let mut last_assistant = None;

    for msg in messages.iter().skip(pre_turn_msg_count.saturating_sub(1)) {
        match msg.role {
            flint_types::Role::User => last_user = Some(msg.text()),
            flint_types::Role::Assistant => {
                let t = msg.text();
                if !t.is_empty() {
                    last_assistant = Some(t);
                }
            }
            _ => {}
        }
    }

    let (user_msg, assistant_msg) = match (last_user, last_assistant) {
        (Some(u), Some(a)) => (u, a),
        _ => return,
    };

    // Skip extraction for very short exchanges
    if user_msg.len() < 20 || assistant_msg.len() < 20 {
        return;
    }

    let mm = memory.lock().unwrap();
    let extract_prompt = mm.extraction_prompt(&user_msg, &assistant_msg);
    drop(mm);

    // Use a temporary session for extraction
    let mut extract_session = Session::new();
    extract_session.add_user(&extract_prompt);

    match run_turn(
        prov.as_ref(),
        &mut extract_session,
        registry,
        "You are a memory extraction system. Output only valid JSON.",
        ctx,
        3,
        None,
        65536,
    )
    .await
    {
        Ok((response, _)) => {
            if response.is_empty() {
                return;
            }

            let mut mm = memory.lock().unwrap();
            let extracted = mm.parse_extracted(&response);
            if !extracted.is_empty() {
                match mm.store_extracted(&extracted, flint_memory::MemoryScope::Project) {
                    Ok(ids) => {
                        if !ids.is_empty() {
                            eprintln!(
                                "\x1b[90m  memory: extracted {} fact(s)\x1b[0m",
                                ids.len()
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("memory extraction storage failed: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!("memory extraction failed: {}", e);
        }
    }
}
