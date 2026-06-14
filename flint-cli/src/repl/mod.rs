//! REPL loop: input reading, command dispatch, LLM interaction, session management.

pub mod auto_poke;
pub mod render;
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
use crate::typeahead;

/// Result of executing a turn.
enum TurnOutcome {
    Success { text: String, tool_calls: u32 },
    Error(String),
}

/// Execute a single LLM turn with the given input.
///
/// This is the core turn execution pattern used by:
/// - Initial message handling
/// - Pending coordinator message handling
/// - Sub-agent result processing
/// - Normal user input
/// - Auto-poke continuation
async fn execute_turn(
    prov: &dyn Provider,
    session: &mut Session,
    registry: &ToolRegistry,
    system: &str,
    ctx: &ToolContext,
    config: &flint_config::Config,
    cancel: &Arc<AtomicBool>,
    swarm_notify_shared: &Option<Arc<Mutex<tokio::sync::mpsc::Receiver<flint_swarm::AgentNotification>>>>,
    swarm: &Option<Arc<Mutex<flint_swarm::SwarmManager>>>,
) -> TurnOutcome {
    let render_line = |line: &str| {
        crate::repl::render::render_markdown_line_to_stdout(line);
    };

    // Set up turn callback for real-time notification draining
    let cb_notify = swarm_notify_shared.clone();
    let cb_swarm = swarm.clone();
    let turn_callback: flint_agent::EventCallback = Box::new(move |_event| {
        drain_and_display_notifications_sync(&cb_notify, &cb_swarm);
        drain_and_display_streams_sync(&cb_swarm);
        true
    });

    match run_turn(
        prov,
        session,
        registry,
        system,
        ctx,
        config.agent.max_turns,
        Some(cancel.clone()),
        config.agent.max_output_chars,
        false,
        Some(&turn_callback),
        Some(&render_line),
    )
    .await
    {
        Ok((_text, stats)) => TurnOutcome::Success {
            text: _text,
            tool_calls: stats.tool_calls,
        },
        Err(e) => TurnOutcome::Error(e.to_string()),
    }
}

/// Check if the session needs compaction and compact if necessary.
/// Returns true if compaction was performed.
async fn maybe_compact(
    session: &mut Session,
    current_session_meta: &mut Option<flint_agent::SessionMeta>,
    prov: &dyn Provider,
    registry: &ToolRegistry,
    ctx: &ToolContext,
    config: &flint_config::Config,
) -> bool {
    if !config.features.is_enabled(Feature::Compaction) {
        return false;
    }

    let total_chars: usize = session.messages.iter().map(|m| m.estimated_chars()).sum();
    let threshold = (config.agent.context_window_chars as f64 * 0.8) as usize;

    if total_chars <= threshold || session.messages.len() <= 6 {
        return false;
    }

    eprintln!(
        "\x1b[90m  auto-compact: {} chars exceeds {} threshold\x1b[0m",
        total_chars, threshold
    );

    let pre_compact_count = session.messages.len();
    dispatch_auto_compact(session, current_session_meta, prov, registry, ctx).await;

    // Save immediately after compaction
    if session.messages.len() < pre_compact_count {
        if let Some(meta) = save_session(session, current_session_meta, config) {
            *current_session_meta = Some(meta);
        }
        true
    } else {
        false
    }
}

/// Print the startup banner with version, provider, and feature info.
fn print_startup_banner(
    config: &flint_config::Config,
    working_dir: &Path,
    memory: &Option<Arc<Mutex<flint_memory::MemoryManager>>>,
    swarm: &Option<Arc<Mutex<flint_swarm::SwarmManager>>>,
    auto_poke: &Option<auto_poke::AutoPoke>,
) {
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

    if let Some(ref sw) = swarm {
        let sm = sw.lock().unwrap();
        println!(
            "Swarm: enabled (max {} agents) -- /swarm to manage",
            sm.config().max_agents
        );
    }

    if auto_poke.is_some() {
        println!("Auto-poke: enabled (todo tool active) -- /poke to toggle");
    }
    println!("Checkpoints: enabled (file snapshots per turn) -- /undo to revert");
    println!();
}

/// Process type-ahead input after agent execution completes.
/// This handles the buffered input as if the user typed it at the prompt.
async fn process_typeahead_input(
    input: &str,
    session: &mut Session,
    current_session_meta: &mut Option<flint_agent::SessionMeta>,
    prov: &Arc<dyn Provider>,
    registry: &ToolRegistry,
    turn_count: &mut u32,
    turn_counter: &Arc<std::sync::atomic::AtomicU32>,
    checkpoint_store: &flint_agent::CheckpointStore,
    ctx: &ToolContext,
    config: &flint_config::Config,
    cancel: &Arc<AtomicBool>,
    swarm_notify_shared: &Option<Arc<Mutex<tokio::sync::mpsc::Receiver<flint_swarm::AgentNotification>>>>,
    swarm: &Option<Arc<Mutex<flint_swarm::SwarmManager>>>,
    auto_poke: &mut Option<auto_poke::AutoPoke>,
    memory: &Option<Arc<Mutex<flint_memory::MemoryManager>>>,
    sub_agent_mode: bool,
    result_delivered: &mut bool,
    router_endpoint: &mut Option<flint_swarm::endpoint::AgentEndpoint>,
    working_dir: &Path,
) {
    // Shell commands
    if input.starts_with('!') {
        crate::repl::shell::execute(&input[1..], working_dir);
        return;
    }

    // Slash commands
    if input.starts_with('/') {
        // For type-ahead, we just print a message - user should use normal prompt
        eprintln!("\x1b[90m  [slash commands not supported in auto-submitted type-ahead]\x1b[0m");
        return;
    }

    // Normal LLM message
    *turn_count += 1;
    turn_counter.store(*turn_count, std::sync::atomic::Ordering::Relaxed);

    flint_agent::checkpoint::set_session_msg_count(
        checkpoint_store, *turn_count, session.messages.len(),
    );

    let effective_system = crate::prompt::build_system_prompt(
        &config.agent.system_prompt.clone().unwrap_or_default(),
        config,
        working_dir,
    );

    // Search for matching skill
    let skill_prompt = config.load_skill_metas(Some(working_dir)).iter()
        .find(|s| s.name != "init" && input.to_lowercase().starts_with(&s.name.to_lowercase()))
        .and_then(|m| config.load_skill_by_name(&m.name, Some(working_dir)))
        .map(|s| s.prompt);

    let mut enhanced_input = String::new();
    if let Some(ref skill) = skill_prompt {
        enhanced_input.push_str(skill);
        enhanced_input.push_str("\n\n");
    }

    // Search memory for relevant context
    if config.features.is_enabled(Feature::Memory) {
        if let Some(ref mem) = memory {
            let mut mem_guard = mem.lock().unwrap();
            let results = mem_guard.search(input, None, Some(3));
            if !results.is_empty() {
                enhanced_input.push_str("[Relevant memories]\n");
                for r in &results {
                    enhanced_input.push_str(&format!("- {}\n", r.entry.content));
                }
                enhanced_input.push('\n');
            }
        }
    }

    enhanced_input.push_str(input);
    session.add_user(&enhanced_input);

    cancel.store(false, Ordering::Relaxed);

    let cb_notify = swarm_notify_shared.clone();
    let cb_swarm = swarm.clone();
    let turn_callback: flint_agent::EventCallback = Box::new(move |_event| {
        drain_and_display_notifications_sync(&cb_notify, &cb_swarm);
        drain_and_display_streams_sync(&cb_swarm);
        true
    });

    let render_line = |line: &str| {
        crate::repl::render::render_markdown_line_to_stdout(line);
    };

    let pre_turn_msg_count = session.messages.len();

    match run_turn(
        prov.as_ref(),
        session,
        registry,
        &effective_system,
        ctx,
        config.agent.max_turns,
        Some(cancel.clone()),
        config.agent.max_output_chars,
        false,
        Some(&turn_callback),
        Some(&render_line),
    )
    .await
    {
        Ok((_text, stats)) => {
            // Auto-extract memories
            if config.features.memory.auto_extract {
                if let Some(ref mem) = memory {
                    spawn_auto_extract(
                        Arc::clone(mem),
                        session.messages.clone(),
                        pre_turn_msg_count,
                        Arc::clone(prov),
                        registry.clone(),
                        ctx.clone(),
                    );
                }
            }

            // Sub-agent task completion
            if sub_agent_mode && !*result_delivered {
                let last_text = session.messages.iter().rev()
                    .find(|m| m.role == flint_types::Role::Assistant)
                    .map(|m| m.text())
                    .unwrap_or_default();
                if last_text.contains("[TASK_COMPLETE]") {
                    let result = last_text.replace("[TASK_COMPLETE]", "").trim().to_string();
                    deliver_sub_agent_result(&result, router_endpoint, working_dir).await;
                    *result_delivered = true;
                    eprintln!("\n\x1b[32m  [task complete] Result delivered.\x1b[0m");
                    eprintln!("\x1b[90m  Waiting for follow-up tasks...\x1b[0m");
                }
            }
        }
        Err(e) => {
            eprintln!("\n\x1b[31m! Error:\x1b[0m {}", e);
        }
    }
}

/// Run the interactive REPL loop.
pub async fn run(
    mut config: flint_config::Config,
    mut prov: Arc<dyn Provider>,
    mut session: Session,
    mut registry: ToolRegistry,
    system: &str,
    ctx: &ToolContext,
    cancel: Arc<AtomicBool>,
    mut mcp_manager: McpManager,
    working_dir: &Path,
    mut memory: Option<Arc<Mutex<flint_memory::MemoryManager>>>,
    mut swarm: Option<Arc<Mutex<flint_swarm::SwarmManager>>>,
    swarm_notify: Option<tokio::sync::mpsc::Receiver<flint_swarm::AgentNotification>>,
    mut auto_poke: Option<auto_poke::AutoPoke>,
    checkpoint_store: flint_agent::CheckpointStore,
    turn_counter: Arc<std::sync::atomic::AtomicU32>,
    initial_message: Option<String>,
    message_file: Option<String>,
    router_addr: Option<String>,
    agent_id: Option<String>,
) -> Result<()> {
    print_startup_banner(&config, working_dir, &memory, &swarm, &auto_poke);

    let mut current_session_meta: Option<flint_agent::SessionMeta> = None;
    let mut turn_count: u32 = 0;
    let mut total_tool_calls: u32 = 0;

    // Detect sub-agent mode (spawned in new terminal)
    let sub_agent_mode = std::env::var("FLINT_SUB_AGENT_ID").is_ok();
    let mut result_delivered = false; // Track if result has been delivered for current task

    // Wrap swarm_notify in Arc<Mutex> so the run_turn callback can drain it
    let swarm_notify_shared: Option<Arc<Mutex<tokio::sync::mpsc::Receiver<flint_swarm::AgentNotification>>>> =
        swarm_notify.map(|rx| Arc::new(Mutex::new(rx)));

    // Get file access notification channel from swarm manager
    let file_access_rx: Option<Arc<Mutex<tokio::sync::mpsc::Receiver<flint_swarm::FileAccessNotification>>>> =
        swarm.as_ref().and_then(|sw| {
            let mut sm = sw.lock().unwrap();
            sm.take_file_access_rx().map(|rx| Arc::new(Mutex::new(rx)))
        });

    // Get router Arc for real-time result checking (coordinator side)
    let router_arc_for_results: Option<Arc<flint_swarm::MessageRouter>> = swarm.as_ref()
        .and_then(|sw| sw.lock().unwrap().router_arc());

    // Background task: drain router results into a shared buffer every 100ms.
    // Stops automatically when the stop flag is set (all agents completed).
    let results_buffer: Arc<Mutex<Vec<flint_swarm::AgentResult>>> =
        Arc::new(Mutex::new(Vec::new()));
    let poll_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(ref router) = router_arc_for_results {
        let buf = results_buffer.clone();
        let r = router.clone();
        let active = poll_active.clone();
        active.store(true, std::sync::atomic::Ordering::Relaxed);
        tokio::spawn(async move {
            while active.load(std::sync::atomic::Ordering::Relaxed) {
                let drained = r.drain_results().await;
                if !drained.is_empty() {
                    buf.lock().unwrap().extend(drained);
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });
    }

    // Connect to router if address provided (for sub-agents)
    let mut router_endpoint: Option<flint_swarm::endpoint::AgentEndpoint> = None;
    if let (Some(ref addr), Some(ref id)) = (&router_addr, &agent_id) {
        match flint_swarm::endpoint::AgentEndpoint::connect(addr, id).await {
            Ok(ep) => {
                eprintln!("\x1b[36m  [router] Connected to message router as {}\x1b[0m", id);
                router_endpoint = Some(ep);
            }
            Err(e) => {
                eprintln!("\x1b[31m  [router] Failed to connect: {}\x1b[0m", e);
            }
        }
    }

    // Process initial message if provided (e.g., from --initial-message flag)
    if let Some(ref msg) = initial_message {
        turn_count += 1;
        turn_counter.store(turn_count, std::sync::atomic::Ordering::Relaxed);
        eprintln!("\x1b[34m{}>\x1b[0m {}", turn_count, msg);
        session.add_user(msg);
        cancel.store(false, Ordering::Relaxed);

        flint_agent::checkpoint::set_session_msg_count(
            &checkpoint_store, turn_count, session.messages.len(),
        );

        let effective_system = system.to_string();
        let outcome = execute_turn(
            prov.as_ref(), &mut session, &registry, &effective_system, ctx,
            &config, &cancel, &swarm_notify_shared, &swarm,
        ).await;

        match outcome {
            TurnOutcome::Success { tool_calls, .. } => {
                total_tool_calls += tool_calls;

                // Sub-agent: check for [TASK_COMPLETE] in initial message response
                if sub_agent_mode {
                    let last_text = session.messages.iter().rev()
                        .find(|m| m.role == flint_types::Role::Assistant)
                        .map(|m| m.text())
                        .unwrap_or_default();
                    if last_text.contains("[TASK_COMPLETE]") {
                        let result = last_text.replace("[TASK_COMPLETE]", "").trim().to_string();
                        deliver_sub_agent_result(&result, &mut router_endpoint, working_dir).await;
                        result_delivered = true;
                        eprintln!("\x1b[32m  [task complete] Result delivered to coordinator.\x1b[0m");
                        eprintln!("\x1b[90m  Waiting for follow-up tasks...\x1b[0m");
                    }
                }
            }
            TurnOutcome::Error(e) => {
                eprintln!("\n\x1b[31m! Error:\x1b[0m {}", e);
            }
        }
        println!();
    }

    loop {
        // Check for pending messages from the coordinator
        // Priority: router (real-time) > file (fallback)
        let pending_message = if let Some(ref mut ep) = router_endpoint {
            // Try non-blocking read from router
            match ep.try_read_message().await {
                Ok(Some(flint_swarm::router::RouterMessage::Incoming { from, content })) => {
                    Some((from, content))
                }
                Ok(Some(flint_swarm::router::RouterMessage::Stop { .. })) => {
                    eprintln!("\x1b[31m  [router] Received stop signal\x1b[0m");
                    break;
                }
                Ok(Some(_)) => None, // Ignore other messages
                Ok(None) => None,
                Err(e) => {
                    eprintln!("\x1b[33m  [router] Read error: {}\x1b[0m", e);
                    None
                }
            }
        } else if let Some(ref msg_path) = message_file {
            // Fallback: file-based communication
            std::fs::read_to_string(msg_path).ok().and_then(|content| {
                let trimmed = content.trim().to_string();
                if !trimmed.is_empty() {
                    let _ = std::fs::write(msg_path, "");
                    Some(("coordinator".to_string(), trimmed))
                } else {
                    None
                }
            })
        } else {
            None
        };

        if let Some((from, content)) = pending_message {
            eprintln!("\x1b[36m  [{}]\x1b[0m {}", from, content);
            result_delivered = false; // New task from coordinator
            session.messages.push(flint_types::Message::system(
                &format!("[Message from {}]: {}", from, content)
            ));
            turn_count += 1;
            turn_counter.store(turn_count, std::sync::atomic::Ordering::Relaxed);
            flint_agent::checkpoint::set_session_msg_count(
                &checkpoint_store, turn_count, session.messages.len(),
            );
            let effective_system = system.to_string();
            let outcome = execute_turn(
                prov.as_ref(), &mut session, &registry, &effective_system, ctx,
                &config, &cancel, &swarm_notify_shared, &swarm,
            ).await;
            if let TurnOutcome::Success { tool_calls, .. } = outcome {
                total_tool_calls += tool_calls;
            }
            println!();
        }

        // Restart polling if agents are active
        if let Some(ref sw) = swarm {
            let active = sw.lock().unwrap().active_agent_count();
            if active > 0 && !poll_active.load(std::sync::atomic::Ordering::Relaxed) {
                poll_active.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Some(ref router) = router_arc_for_results {
                    let buf = results_buffer.clone();
                    let r = router.clone();
                    let pa = poll_active.clone();
                    tokio::spawn(async move {
                        while pa.load(std::sync::atomic::Ordering::Relaxed) {
                            let drained = r.drain_results().await;
                            if !drained.is_empty() {
                                buf.lock().unwrap().extend(drained);
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    });
                }
            }
        }

        print!("\u{276f} ");
        std::io::stdout().flush()?;

        // Use read_line_with_handler to process sub-agent input requests
        // and pending results while waiting for user keystrokes.
        // The handler is called every ~100ms during the poll timeout.
        // Check for pending sub-agent results BEFORE waiting for input.
        // If results arrived while idle, process them immediately without waiting for Enter.
        let mut collected_results: Vec<flint_swarm::AgentResult> =
            results_buffer.lock().unwrap().drain(..).collect();

        let input = if !collected_results.is_empty() {
            // Results pending — skip input, process results immediately
            for r in &collected_results {
                let short_id = r.agent_id.strip_prefix("agent_").unwrap_or(&r.agent_id);
                let short_id = &short_id[..4.min(short_id.len())];
                eprintln!("\n\x1b[36m  [result from {}] received\x1b[0m", short_id);
            }
            String::new()
        } else {
            // No results — wait for user input
            let handler_swarm = &swarm;
            let handler_buf = results_buffer.clone();
            let handler_collected = Arc::new(Mutex::new(Vec::<flint_swarm::AgentResult>::new()));
            let hc = handler_collected.clone();
            match crate::input::read_line_with_handler(|| {
                // Drain input requests from sub-agents
                let requests = if let Some(ref sw) = handler_swarm {
                    let sm = sw.lock().unwrap();
                    sm.drain_input_requests()
                } else {
                    return;
                };
                for req in &requests {
                    let short_id = req.agent_id.strip_prefix("agent_").unwrap_or(&req.agent_id);
                    let short_id = &short_id[..4.min(short_id.len())];
                    let _ = crossterm::terminal::disable_raw_mode();
                    eprintln!();
                    eprintln!("\x1b[36m  [{}] asks:\x1b[0m {}", short_id, req.prompt);
                    print!("\x1b[36m  > \x1b[0m");
                    let _ = std::io::stdout().flush();
                    let mut response = String::new();
                    if std::io::stdin().read_line(&mut response).is_ok() {
                        let response_text = response.trim().to_string();
                        let response_tx = {
                            let sm = handler_swarm.as_ref().unwrap().lock().unwrap();
                            sm.get_input_response_tx(&req.agent_id)
                        };
                        if let Some(tx) = response_tx {
                            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                                let _ = rt.block_on(tx.send(
                                    flint_swarm::InputResponse { text: response_text }
                                ));
                            }
                        }
                    }
                    let _ = crossterm::terminal::enable_raw_mode();
                    print!("\u{276f} ");
                    let _ = std::io::stdout().flush();
                }
                // Check results buffer during input wait
                if let Ok(mut buf) = handler_buf.try_lock() {
                    if !buf.is_empty() {
                        let drained: Vec<_> = buf.drain(..).collect();
                        let _ = crossterm::terminal::disable_raw_mode();
                        for r in &drained {
                            let short_id = r.agent_id.strip_prefix("agent_").unwrap_or(&r.agent_id);
                            let short_id = &short_id[..4.min(short_id.len())];
                            eprintln!("\n\x1b[36m  [result from {}] received\x1b[0m", short_id);
                        }
                        let _ = crossterm::terminal::enable_raw_mode();
                        hc.lock().unwrap().extend(drained);
                    }
                }
            })? {
                crate::input::InputResult::Line(line) => {
                    // Merge any results collected during input wait
                    collected_results.extend(handler_collected.lock().unwrap().drain(..));
                    line
                }
                crate::input::InputResult::Exit => {
                    println!("Bye.");
                    break;
                }
            }
        };
        if !collected_results.is_empty() {
            for r in collected_results {
                {
                    let mut sm = swarm.as_ref().unwrap().lock().unwrap();
                    sm.complete_task(&r.task_id, &r.result, true);
                }
                let short_id = r.agent_id.strip_prefix("agent_").unwrap_or(&r.agent_id);
                let short_id = &short_id[..4.min(short_id.len())];
                let result_msg = format!(
                    "[Sub-agent {} completed task {}]\n{}",
                    short_id, &r.task_id[..8.min(r.task_id.len())], r.result
                );
                session.messages.push(flint_types::Message::system(&result_msg));
                turn_count += 1;
                turn_counter.store(turn_count, std::sync::atomic::Ordering::Relaxed);
                flint_agent::checkpoint::set_session_msg_count(
                    &checkpoint_store, turn_count, session.messages.len(),
                );
                let effective_system = system.to_string();
                let outcome = execute_turn(
                    prov.as_ref(), &mut session, &registry, &effective_system, ctx,
                    &config, &cancel, &swarm_notify_shared, &swarm,
                ).await;
                if let TurnOutcome::Success { tool_calls, .. } = outcome {
                    total_tool_calls += tool_calls;
                }
                println!();
            }
            // Check if all agents are done — stop polling
            if let Some(ref sw) = swarm {
                let active = sw.lock().unwrap().active_agent_count();
                if active == 0 {
                    poll_active.store(false, std::sync::atomic::Ordering::Relaxed);
                }
            }
            continue; // Re-show prompt after processing results
        }

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
                registry: &mut registry,
                ctx,
                _cancel: &cancel,
                mcp_manager: &mut mcp_manager,
                working_dir,
                memory: &mut memory,
                swarm: &mut swarm,
                auto_poke: &mut auto_poke,
                checkpoint_store: checkpoint_store.clone(),
                _turn_counter: turn_counter.clone(),
                arg: None,
                system,
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

        // Natural language undo detection
        if is_undo_request(&input) {
            perform_undo(
                &checkpoint_store,
                &mut session,
                working_dir,
            );
            continue;
        }

        // Normal message -> send to LLM
        turn_count += 1;
        turn_counter.store(turn_count, std::sync::atomic::Ordering::Relaxed);

        // Record session message count before this turn (for rollback)
        flint_agent::checkpoint::set_session_msg_count(
            &checkpoint_store,
            turn_count,
            session.messages.len(),
        );

        eprintln!("\x1b[34m{}>\x1b[0m {}", turn_count, input);

        // Reset auto-poke counter on user input
        if let Some(ref mut ap) = auto_poke {
            ap.reset_counter();
        }

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

        // Pre-send compaction: compact BEFORE the API call if session is too large.
        maybe_compact(&mut session, &mut current_session_meta, &*prov, &registry, ctx, &config).await;

        // Remember the last assistant message index for extraction
        let pre_turn_msg_count = session.messages.len();

        // Callback that drains sub-agent notifications and stream output in real-time
        // during run_turn, so the user sees progress without waiting for the turn to end.
        let cb_notify = swarm_notify_shared.clone();
        let cb_swarm = swarm.clone();
        let cb_file_access = file_access_rx.clone();
        let turn_callback: flint_agent::EventCallback = Box::new(move |_event| {
            drain_and_display_notifications_sync(&cb_notify, &cb_swarm);
            drain_and_display_streams_sync(&cb_swarm);
            drain_file_access_notifications(&cb_file_access, &cb_swarm);
            true
        });

        let render_line = |line: &str| {
            crate::repl::render::render_markdown_line_to_stdout(line);
        };

        // Spawn typeahead reader thread to capture input during agent execution
        let typeahead_buf = Arc::new(Mutex::new(typeahead::TypeaheadBuffer::new()));
        let (typeahead_handle, typeahead_stop) = typeahead::spawn_typeahead_reader(
            typeahead_buf.clone(),
            cancel.clone(),
        );

        match run_turn(
            prov.as_ref(),
            &mut session,
            &registry,
            &effective_system,
            ctx,
            config.agent.max_turns,
            Some(cancel.clone()),
            config.agent.max_output_chars,
            false,
            Some(&turn_callback),
            Some(&render_line),
        )
        .await
        {
            Ok((_text, stats)) => {
                total_tool_calls += stats.tool_calls;

                // Auto-extract memories from the conversation (if enabled)
                // Spawns as a background task — does not block the REPL.
                if config.features.memory.auto_extract {
                    if let Some(ref mem) = memory {
                        spawn_auto_extract(
                            Arc::clone(mem),
                            session.messages.clone(),
                            pre_turn_msg_count,
                            Arc::clone(&prov),
                            registry.clone(),
                            ctx.clone(),
                        );
                    }
                }

                // Sub-agent task completion: check if the last assistant message
                // contains [TASK_COMPLETE]. If so, deliver result but stay alive
                // for follow-up tasks from the coordinator.
                if sub_agent_mode && !result_delivered {
                    let last_text = session.messages.iter().rev()
                        .find(|m| m.role == flint_types::Role::Assistant)
                        .map(|m| m.text())
                        .unwrap_or_default();
                    if last_text.contains("[TASK_COMPLETE]") {
                        let result = last_text.replace("[TASK_COMPLETE]", "").trim().to_string();
                        deliver_sub_agent_result(&result, &mut router_endpoint, working_dir).await;
                        result_delivered = true;
                        eprintln!("\n\x1b[32m  [task complete] Result delivered.\x1b[0m");
                        eprintln!("\x1b[90m  Waiting for follow-up tasks...\x1b[0m");
                    }
                }

                // ── Inject sub-agent notifications into session ──────────
                // After each turn, drain any pending notifications and inject
                // them as system messages so the main agent knows about
                // sub-agent completions and failures.
                let notifications = drain_and_display_notifications(&swarm_notify_shared, &swarm);
                for notif in &notifications {
                    let short_id = notif.agent_id.strip_prefix("agent_").unwrap_or(&notif.agent_id);
                    let short_id = &short_id[..4.min(short_id.len())];
                    let short_task = &notif.task_id[..8.min(notif.task_id.len())];
                    let msg = match &notif.result {
                        Ok(text) => {
                            format!("[Sub-agent {} completed task {}]\n{}", short_id, short_task, text)
                        }
                        Err(e) => {
                            format!(
                                "[Sub-agent {} FAILED task {}]\nError: {}\n\n\
                                 You should handle this task yourself or spawn a new agent to retry.",
                                short_id, short_task, e
                            )
                        }
                    };
                    session.messages.push(flint_types::Message::system(&msg));
                }

                // ── Auto-poke: keep going if incomplete todos remain ──────
                // This loop continues until: all todos complete, max pokes
                // reached, a non-retryable error occurs, or the turn fails.
                loop {
                    let poke_msg = match auto_poke {
                        Some(ref mut ap) => ap.should_poke(),
                        None => break,
                    };
                    let poke_msg = match poke_msg {
                        Some(m) => m,
                        None => break,
                    };

                    turn_count += 1;
                    turn_counter.store(turn_count, std::sync::atomic::Ordering::Relaxed);
                    eprintln!(
                        "\x1b[33m  [auto-poke {}/{}]\x1b[0m {}",
                        auto_poke.as_ref().map(|a| a.consecutive_pokes).unwrap_or(0),
                        auto_poke.as_ref().map(|a| a.max_pokes).unwrap_or(10),
                        poke_msg
                    );

                    session.add_user(&poke_msg);

                    flint_agent::checkpoint::set_session_msg_count(
                        &checkpoint_store, turn_count, session.messages.len(),
                    );

                    match run_turn(
                        prov.as_ref(),
                        &mut session,
                        &registry,
                        &effective_system,
                        ctx,
                        config.agent.max_turns,
                        Some(cancel.clone()),
                        config.agent.max_output_chars,
                        false,
                        Some(&turn_callback),
                        Some(&render_line),
                    )
                    .await
                    {
                        Ok((_poke_text, poke_stats)) => {
                            total_tool_calls += poke_stats.tool_calls;
                            // Continue loop — check if more pokes needed
                        }
                        Err(e) => {
                            eprintln!("\n\x1b[31m! Error during auto-poke:\x1b[0m {}", e);
                            if let Some(ref mut ap) = auto_poke {
                                ap.stop_for_error(&e.to_string());
                            }
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("\n\x1b[31m! Error:\x1b[0m {}", e);
                eprintln!("  Type /setup to reconfigure provider, or /model to switch model.\n");
                // Stop auto-poke on non-retryable errors
                if let Some(ref mut ap) = auto_poke {
                    ap.stop_for_error(&e.to_string());
                }
            }
        }

        // Stop typeahead reader thread and process buffered input
        typeahead_stop.store(true, Ordering::Relaxed);
        let _ = typeahead_handle.join();

        {
            let buf = typeahead_buf.lock().unwrap();
            if buf.is_cancelled() {
                // User pressed Ctrl+C during execution - buffer discarded
                eprintln!();
            } else if !buf.is_empty() {
                let buffered_text = buf.text().to_string();
                let was_submitted = buf.is_submitted();
                drop(buf);

                if was_submitted {
                    // Auto-submit: user pressed Enter, process directly
                    eprintln!();
                    eprintln!("\x1b[90m  ── type-ahead input (auto-submitted) ──\x1b[0m");
                    let input = buffered_text.trim().to_string();
                    if !input.is_empty() {
                        // Process the buffered input as a new turn
                        process_typeahead_input(
                            &input,
                            &mut session,
                            &mut current_session_meta,
                            &prov,
                            &registry,
                            &mut turn_count,
                            &turn_counter,
                            &checkpoint_store,
                            ctx,
                            &config,
                            &cancel,
                            &swarm_notify_shared,
                            &swarm,
                            &mut auto_poke,
                            &memory,
                            sub_agent_mode,
                            &mut result_delivered,
                            &mut router_endpoint,
                            working_dir,
                        ).await;
                    }
                } else {
                    // Present for review: let user edit before submitting
                    eprintln!();
                    eprintln!("\x1b[90m  ── type-ahead input (edit and press Enter to submit) ──\x1b[0m");
                    print!("❯ ");
                    std::io::stdout().flush()?;
                    match crate::input::read_line_prefilled(&buffered_text) {
                        Ok(crate::input::InputResult::Line(line)) => {
                            let input = line.trim().to_string();
                            if !input.is_empty() {
                                process_typeahead_input(
                                    &input,
                                    &mut session,
                                    &mut current_session_meta,
                                    &prov,
                                    &registry,
                                    &mut turn_count,
                                    &turn_counter,
                                    &checkpoint_store,
                                    ctx,
                                    &config,
                                    &cancel,
                                    &swarm_notify_shared,
                                    &swarm,
                                    &mut auto_poke,
                                    &memory,
                                    sub_agent_mode,
                                    &mut result_delivered,
                                    &mut router_endpoint,
                                    working_dir,
                                ).await;
                            }
                        }
                        Ok(crate::input::InputResult::Exit) => {
                            break;
                        }
                        Err(_) => {}
                    }
                }
            }
        }

        // Drain sub-agent completion notifications and display to user
        drain_and_display_notifications(
            &swarm_notify_shared,
            &swarm,
        );

        // Drain file access notifications from sub-agents
        drain_file_access_notifications(
            &file_access_rx,
            &swarm,
        );

        // Drain streaming output from interactive agents
        if let Some(ref sw) = swarm {
            let stream_chunks = {
                let sm = sw.lock().unwrap();
                sm.drain_all_streams()
            };
            for (agent_id, chunk) in &stream_chunks {
                let short_id = agent_id.strip_prefix("agent_").unwrap_or(agent_id);
                let short_id = &short_id[..4.min(short_id.len())];

                if chunk.starts_with("[INPUT_REQUESTED:") {
                    let prompt = chunk
                        .strip_prefix("[INPUT_REQUESTED:")
                        .unwrap_or(chunk)
                        .trim_end_matches(']');
                    eprintln!();
                    eprintln!("\x1b[36m  [{}] asks:\x1b[0m {}", short_id, prompt);
                    print!("\x1b[36m  > \x1b[0m");
                    std::io::stdout().flush()?;
                    let user_input = match crate::input::read_line()? {
                        crate::input::InputResult::Line(line) => line,
                        crate::input::InputResult::Exit => String::new(),
                    };
                    let response_tx = {
                        let sm = sw.lock().unwrap();
                        sm.get_input_response_tx(agent_id)
                    };
                    if let Some(tx) = response_tx {
                        let _ = tx.send(flint_swarm::InputResponse { text: user_input }).await;
                    }
                } else if chunk == "[DONE]" {
                    eprintln!("\x1b[32m  [{}] Done.\x1b[0m", short_id);
                } else if chunk.starts_with("[TOOL:") {
                    eprintln!("\x1b[90m  {}\x1b[0m", chunk);
                } else {
                    let mut buf = Vec::new();
                    render::render_markdown(&mut buf, chunk);
                    let rendered = String::from_utf8_lossy(&buf);
                    print!("{}", rendered);
                    std::io::stdout().flush()?;
                }
            }
        }

        // Auto-compaction: if session exceeds 80% of context window, compact automatically
        maybe_compact(&mut session, &mut current_session_meta, &*prov, &registry, ctx, &config).await;

        // Auto-save session
        if let Some(meta) = save_session(&session, &current_session_meta, &config) {
            current_session_meta = Some(meta);
        }
    }

    mcp_manager.shutdown().await;
    Ok(())
}

/// Save session to disk if persistence is enabled.
/// Returns a new SessionMeta if one was created (first save).
fn save_session(
    session: &Session,
    current_session_meta: &Option<flint_agent::SessionMeta>,
    config: &flint_config::Config,
) -> Option<flint_agent::SessionMeta> {
    if !config.session.persistence || session.is_empty() {
        return None;
    }
    let session_dir = &config.session.path;
    if !session_dir.exists() {
        let _ = std::fs::create_dir_all(session_dir);
    }
    match current_session_meta {
        Some(meta) => {
            let path = session_dir.join(format!("{}.json", meta.id));
            if let Err(e) = session.update_save(&path, meta) {
                eprintln!("Warning: Failed to save session: {}", e);
            }
            None
        }
        None => {
            let path = session_dir.join(format!("{}.json", uuid::Uuid::new_v4()));
            if let Err(e) =
                session.save(&path, &config.provider.r#type, &config.provider.model)
            {
                eprintln!("Warning: Failed to save session: {}", e);
                None
            } else {
                flint_agent::Session::load(&path).ok().map(|(_, meta)| meta)
            }
        }
    }
}

/// Auto-compaction: summarize session and replace with compact form.
async fn dispatch_auto_compact(
    session: &mut Session,
    current_session_meta: &mut Option<flint_agent::SessionMeta>,
    prov: &dyn Provider,
    registry: &ToolRegistry,
    ctx: &ToolContext,
) {
    let msg_count = session.messages.len();

    // Use estimated_chars for accurate size including tool blocks
    let total_estimated: usize = session.messages.iter().map(|m| m.estimated_chars()).sum();

    let mut history = String::new();
    for msg in &session.messages {
        let role = match msg.role {
            flint_types::Role::User => "User",
            flint_types::Role::Assistant => "Assistant",
            flint_types::Role::System => "System",
            flint_types::Role::Tool => "Tool",
        };
        // Include tool block info in the summary for better context preservation
        for block in &msg.content {
            let text = match block {
                flint_types::ContentBlock::Text { text } => text.clone(),
                flint_types::ContentBlock::ToolUse { name, input, .. } => {
                    format!("[tool call: {}({})]", name, input)
                }
                flint_types::ContentBlock::ToolResult { content, is_error, .. } => {
                    if is_error == &Some(true) {
                        format!("[tool error: {}]", content)
                    } else {
                        content.clone()
                    }
                }
            };
            if !text.is_empty() {
                history.push_str(&format!("{}: {}\n\n", role, text));
            }
        }
    }

    // Truncate history if too large for the summarizer to handle
    // Leave room for the summarizer prompt and response
    const MAX_SUMMARY_INPUT: usize = 200_000;
    if history.len() > MAX_SUMMARY_INPUT {
        let truncate_at = history.char_indices()
            .nth(MAX_SUMMARY_INPUT)
            .map(|(i, _)| i)
            .unwrap_or(history.len());
        history = format!(
            "{}\n\n[... truncated from {} to {} chars for summarization ...]",
            &history[..truncate_at],
            total_estimated,
            MAX_SUMMARY_INPUT
        );
    }

    let compact_prompt = format!(
        "Summarize the following conversation concisely. Keep all key facts, decisions, file paths, and code context. Output only the summary, no preamble.\n\n{}",
        history
    );

    let mut compact_session = Session::new();
    compact_session.add_user(&compact_prompt);
    match run_turn(
        prov,
        &mut compact_session,
        registry,
        "You are a summarizer. Be concise.",
        ctx,
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
/// Spawn memory extraction as a background task so the REPL returns to the
/// prompt immediately. The extraction reads the last user/assistant exchange
/// from `messages`, calls the LLM, and stores any extracted facts.
fn spawn_auto_extract(
    memory: Arc<Mutex<flint_memory::MemoryManager>>,
    messages: Vec<flint_types::Message>,
    pre_turn_msg_count: usize,
    prov: Arc<dyn Provider>,
    registry: ToolRegistry,
    ctx: ToolContext,
) {
    // Find the last user message and assistant response
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

    let extract_prompt = {
        let mm = memory.lock().unwrap();
        mm.extraction_prompt(&user_msg, &assistant_msg)
    };

    tokio::spawn(async move {
        let mut extract_session = Session::new();
        extract_session.add_user(&extract_prompt);

        match run_turn(
            prov.as_ref(),
            &mut extract_session,
            &registry,
            "You are a memory extraction system. Output only valid JSON.",
            &ctx,
            3,
            None,
            65536,
            true, // silent
            None,
            None,
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
    });
}

/// Display and interact with an interactive agent's streaming output.
/// Called after `swarm spawn` creates an interactive agent.
/// Polls the agent's stream channel, displays output with markdown rendering,
/// and forwards user input when the agent requests it.
pub async fn _display_agent_output(
    swarm: &std::sync::Arc<std::sync::Mutex<flint_swarm::SwarmManager>>,
    agent_id: &str,
) -> Result<()> {
    let short_id = agent_id.strip_prefix("agent_").unwrap_or(agent_id);
    let short_id = &short_id[..4.min(short_id.len())];

    eprintln!("\x1b[36m  [{}] Working...\x1b[0m", short_id);

    loop {
        // Drain streaming output
        let chunks = {
            let sm = swarm.lock().unwrap();
            sm.drain_stream(agent_id)
        };

        let mut done = false;
        for chunk in &chunks {
            // Check for special markers
            if chunk.starts_with("[INPUT_REQUESTED:") {
                // Agent needs user input
                let prompt = chunk
                    .strip_prefix("[INPUT_REQUESTED:")
                    .unwrap_or(chunk)
                    .trim_end_matches(']');
                eprintln!();
                eprintln!("\x1b[36m  [{}] asks:\x1b[0m {}", short_id, prompt);
                print!("\x1b[36m  > \x1b[0m");
                std::io::stdout().flush()?;

                let user_input = match crate::input::read_line()? {
                    crate::input::InputResult::Line(line) => line,
                    crate::input::InputResult::Exit => String::new(),
                };

                // Send response to the agent
                let response_tx = {
                    let sm = swarm.lock().unwrap();
                    sm.get_input_response_tx(agent_id)
                };
                if let Some(tx) = response_tx {
                    let _ = tx.send(flint_swarm::InputResponse { text: user_input }).await;
                }
            } else if chunk == "[DONE]" {
                done = true;
            } else if chunk.starts_with("[TOOL:") {
                // Tool summary — render dimmed
                eprintln!("\x1b[90m  {}\x1b[0m", chunk);
            } else {
                // Regular text — render with markdown
                let mut buf = Vec::new();
                render::render_markdown(&mut buf, chunk);
                let rendered = String::from_utf8_lossy(&buf);
                print!("{}", rendered);
                std::io::stdout().flush()?;
            }
        }

        if done {
            eprintln!();
            eprintln!("\x1b[32m  [{}] Done.\x1b[0m", short_id);
            break;
        }

        // Small delay to avoid busy-waiting
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    Ok(())
}

/// Display client mode: thin terminal that connects to a swarm agent via router.
///
/// This is the jcode-style architecture: the agent runs as a tokio task on the
/// server, and this client displays its output and forwards user input — all
/// communication happens over TCP via the MessageRouter.
pub async fn run_display_mode(router_addr: &str, agent_id: &str) -> Result<()> {
    use flint_swarm::endpoint::AgentEndpoint;
    use flint_swarm::router::RouterMessage;
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Connect to the router
    let mut ep = AgentEndpoint::connect(router_addr, agent_id).await?;

    let short_id = agent_id.strip_prefix("agent_").unwrap_or(agent_id);
    let short_id = &short_id[..4.min(short_id.len())];
    eprintln!("\x1b[36m  [{}] Connected to agent via router\x1b[0m", short_id);
    eprintln!("\x1b[90m  Type messages to send to the agent. Ctrl+C to exit.\x1b[0m\n");

    // Async stdin reader
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdin_lines = stdin.lines();

    // Prompt state
    let mut needs_prompt = true;

    loop {
        if needs_prompt {
            print!("\x1b[33m> \x1b[0m");
            use std::io::Write;
            std::io::stdout().flush()?;
            needs_prompt = false;
        }

        tokio::select! {
            // Messages from the agent via router
            msg = ep.read_message() => {
                match msg {
                    Ok(RouterMessage::Incoming { from, content }) => {
                        // Check for special prefixes
                        if content.starts_with("[INPUT_REQUESTED:") {
                            // Agent needs user input — show the prompt
                            let prompt = content
                                .strip_prefix("[INPUT_REQUESTED:")
                                .unwrap_or(&content)
                                .trim_end_matches(']');
                            eprintln!();
                            eprintln!("\x1b[36m  [{}] asks:\x1b[0m {}", &from[from.len()-4..], prompt);
                            needs_prompt = true;
                        } else if content.starts_with("[TOOL:") {
                            // Tool call summary
                            eprintln!("\x1b[90m  {}\x1b[0m", content);
                            needs_prompt = true;
                        } else if content.starts_with("[DONE]") {
                            // Agent finished
                            eprintln!();
                            eprintln!("\x1b[32m  [{}] Task complete.\x1b[0m", short_id);
                            break;
                        } else if content.starts_with("[STARTED]") {
                            eprintln!("\x1b[36m  [{}] Started working...\x1b[0m", short_id);
                        } else {
                            // Regular output — render with markdown formatting
                            use std::io::Write;
                            let mut buf = Vec::new();
                            render::render_markdown(&mut buf, &content);
                            let rendered = String::from_utf8_lossy(&buf);
                            print!("{}", rendered);
                            std::io::stdout().flush()?;
                            if content.ends_with('\n') {
                                needs_prompt = true;
                            }
                        }
                    }
                    Ok(RouterMessage::Stop { .. }) => {
                        eprintln!("\n\x1b[31m  [{}] Stopped.\x1b[0m", short_id);
                        break;
                    }
                    Ok(_) => {} // Ignore other messages
                    Err(e) => {
                        eprintln!("\n\x1b[31m  Connection lost: {}\x1b[0m", e);
                        break;
                    }
                }
            }

            // User input from stdin
            line = stdin_lines.next_line() => {
                match line {
                    Ok(Some(text)) => {
                        if text.is_empty() {
                            needs_prompt = true;
                            continue;
                        }
                        // Send input to the agent via router
                        if let Err(e) = ep.send_to(agent_id, &text).await {
                            eprintln!("\n\x1b[31m  Send failed: {}\x1b[0m", e);
                            break;
                        }
                        needs_prompt = true;
                    }
                    Ok(None) => break, // EOF
                    Err(e) => {
                        eprintln!("\n\x1b[31m  stdin error: {}\x1b[0m", e);
                        break;
                    }
                }
            }
        }
    }

    eprintln!("\n\x1b[90m  Display client disconnected.\x1b[0m");
    Ok(())
}

// ── Undo detection and execution ─────────────────────────────────────────

/// Check if user input is a natural language undo request.
fn is_undo_request(input: &str) -> bool {
    let trimmed = input.trim();
    // Slash command
    if trimmed == "/undo" {
        return true;
    }
    // Natural language patterns (Chinese + English)
    let patterns = [
        "回退", "撤销", "回滚", "退回上一轮", "退回上一步",
        "撤销上一轮", "撤销上一步", "回退上一轮", "回退上一步",
        "undo", "revert", "rollback",
        "撤销修改", "回退修改", "撤销代码", "回退代码",
        "撤销刚才", "回退刚才", "撤销这次", "回退这次",
    ];
    let lower = trimmed.to_lowercase();
    patterns.iter().any(|p| lower.contains(p))
}

/// Perform undo: restore files and truncate session messages.
fn perform_undo(
    checkpoint_store: &flint_agent::CheckpointStore,
    session: &mut flint_agent::Session,
    working_dir: &std::path::Path,
) {
    let count = flint_agent::checkpoint::checkpoint_count(checkpoint_store);
    if count == 0 {
        println!("Nothing to undo.\n");
        return;
    }

    let cp = flint_agent::checkpoint::pop_latest(checkpoint_store).unwrap();
    let turn = cp.turn_number;
    let _file_count = cp.snapshots.len();
    let mut restored = 0;
    let mut deleted = 0;

    for snap in &cp.snapshots {
        let full = working_dir.join(&snap.path);
        match &snap.original_content {
            Some(content) => {
                if let Err(e) = std::fs::write(&full, content) {
                    eprintln!("  x failed to restore {}: {}", snap.path.display(), e);
                } else {
                    println!("  + restored {}", snap.path.display());
                    restored += 1;
                }
            }
            None => {
                if full.exists() {
                    if let Err(e) = std::fs::remove_file(&full) {
                        eprintln!("  x failed to delete {}: {}", snap.path.display(), e);
                    } else {
                        println!("  - deleted {}", snap.path.display());
                        deleted += 1;
                    }
                }
            }
        }
    }

    // Truncate session messages back to before this turn
    let msg_before = session.messages.len();
    if cp.session_msg_count < msg_before {
        session.messages.truncate(cp.session_msg_count);
        let removed = msg_before - cp.session_msg_count;
        println!("  ~ truncated {} message(s) from conversation", removed);
    }

    println!(
        "\nUndo turn {}: {} file(s) restored, {} deleted, {} checkpoint(s) remaining.\n",
        turn, restored, deleted, count - 1
    );
}

// ── Notification drain helpers ───────────────────────────────────────────

/// Drain sub-agent completion notifications and display to user.
/// Used between REPL turns (post-turn).
fn drain_and_display_notifications(
    notify_shared: &Option<Arc<Mutex<tokio::sync::mpsc::Receiver<flint_swarm::AgentNotification>>>>,
    swarm: &Option<Arc<Mutex<flint_swarm::SwarmManager>>>,
) -> Vec<flint_swarm::AgentNotification> {
    let notifications: Vec<flint_swarm::AgentNotification> = if let Some(ref rx) = notify_shared {
        let mut rx = rx.lock().unwrap();
        let mut notifs = Vec::new();
        while let Ok(n) = rx.try_recv() {
            notifs.push(n);
        }
        notifs
    } else {
        return Vec::new();
    };

    for notif in &notifications {
        let short_id = notif.agent_id.strip_prefix("agent_").unwrap_or(&notif.agent_id);
        let short_id = &short_id[..4.min(short_id.len())];
        let short_task = &notif.task_id[..8.min(notif.task_id.len())];
        match &notif.result {
            Ok(text) => {
                let preview: String = text.chars().take(200).collect();
                let truncated = if text.len() > 200 { "..." } else { "" };
                eprintln!(
                    "\x1b[36m  [swarm] Agent [{}] completed task {}:\x1b[0m",
                    short_id, short_task
                );
                eprintln!("\x1b[36m  {}{}\x1b[0m", preview, truncated);
                eprintln!(
                    "\x1b[90m  Use 'swarm wait agent_id={}' or 'swarm result task_id={}' to get full result.\x1b[0m",
                    notif.agent_id, notif.task_id
                );
            }
            Err(e) => {
                eprintln!(
                    "\x1b[31m  [swarm] Agent [{}] failed task {}: {}\x1b[0m",
                    short_id, short_task, e
                );
            }
        }
    }

    if let Some(ref sw) = swarm {
        let mut sm = sw.lock().unwrap();
        for notif in &notifications {
            match &notif.result {
                Ok(text) => sm.complete_task(&notif.task_id, text, true),
                Err(e) => sm.complete_task(&notif.task_id, e, false),
            }
        }
    }

    notifications
}

/// Same as drain_and_display_notifications but usable inside a Fn callback
/// (takes shared references that can be cloned into closures).
fn drain_and_display_notifications_sync(
    notify_shared: &Option<Arc<Mutex<tokio::sync::mpsc::Receiver<flint_swarm::AgentNotification>>>>,
    swarm: &Option<Arc<Mutex<flint_swarm::SwarmManager>>>,
) {
    drain_and_display_notifications(notify_shared, swarm);
}

/// Drain file access notifications from sub-agents and update the tracker.
fn drain_file_access_notifications(
    file_access_rx: &Option<Arc<Mutex<tokio::sync::mpsc::Receiver<flint_swarm::FileAccessNotification>>>>,
    swarm: &Option<Arc<Mutex<flint_swarm::SwarmManager>>>,
) {
    if let Some(ref rx) = file_access_rx {
        let mut rx = rx.lock().unwrap();
        while let Ok(notification) = rx.try_recv() {
            if let Some(ref sw) = swarm {
                let mut sm = sw.lock().unwrap();
                sm.file_access_tracker_mut().record_access(
                    &notification.agent_id,
                    &notification.path,
                );
            }
        }
    }
}

/// Drain streaming output from interactive agents during run_turn callback.
fn drain_and_display_streams_sync(
    swarm: &Option<Arc<Mutex<flint_swarm::SwarmManager>>>,
) {
    if let Some(ref sw) = swarm {
        let stream_chunks = {
            let sm = sw.lock().unwrap();
            sm.drain_all_streams()
        };
        for (agent_id, chunk) in &stream_chunks {
            let short_id = agent_id.strip_prefix("agent_").unwrap_or(agent_id);
            let short_id = &short_id[..4.min(short_id.len())];

            if chunk.starts_with("[INPUT_REQUESTED:") {
                // Input requests are handled by the REPL's input handler
                // (read_line_with_handler), so skip here.
            } else if chunk == "[DONE]" {
                eprintln!("\x1b[32m  [{}] Done.\x1b[0m", short_id);
            } else if chunk.starts_with("[TOOL:") {
                eprintln!("\x1b[90m  {}\x1b[0m", chunk);
            } else {
                // Regular text — render with markdown
                let mut buf = Vec::new();
                render::render_markdown(&mut buf, chunk);
                let rendered = String::from_utf8_lossy(&buf);
                use std::io::Write;
                print!("{}", rendered);
                let _ = std::io::stdout().flush();
            }
        }
    }
}

// ── Sub-agent result delivery ───────────────────────────────────────────

/// Deliver the sub-agent's final result to the coordinator via TCP Router.
/// The coordinator's `swarm wait` polls the router's result channel to receive this.
async fn deliver_sub_agent_result(
    result: &str,
    router_endpoint: &mut Option<flint_swarm::endpoint::AgentEndpoint>,
    _working_dir: &Path,
) {
    let agent_id = std::env::var("FLINT_SUB_AGENT_ID").unwrap_or_default();
    let task_id = std::env::var("FLINT_SUB_TASK_ID").unwrap_or_default();
    if let Some(ep) = router_endpoint.as_mut() {
        if !agent_id.is_empty() {
            let msg = flint_swarm::router::RouterMessage::Result {
                agent_id,
                task_id,
                result: result.to_string(),
            };
            let _ = ep.send_message(&msg).await;
        }
    }
}
