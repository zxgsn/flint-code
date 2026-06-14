//! Sub-agent runtime — two modes:
//!
//! 1. **In-process** (default): tokio task, output to log file, viewer terminal
//! 2. **External process**: new terminal running `flint` REPL, fully interactive

use crate::log;
use crate::output::{OutputEvent, OutputSender};
use crate::router::MessageRouter;
use crate::tool::INPUT_REQUESTED_PREFIX;
use crate::types::AgentNotification;
use flint_agent::{AgentEvent, Session, ToolContext, ToolRegistry};
use flint_provider::Provider;
use flint_types::StreamEvent;
use futures::StreamExt;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

/// Build the base sub-agent registry (file/shell tools only).
/// Used when no coordinator registry is available.
pub fn build_sub_agent_registry(
    agent_id: &str,
    file_access_tx: mpsc::Sender<crate::types::FileAccessNotification>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    crate::tool::register_sub_agent_tools(&mut registry, agent_id, file_access_tx);
    registry
}

fn fmt_elapsed(ms: u64) -> String {
    if ms < 1000 { format!("{}ms", ms) } else { format!("{:.1}s", ms as f64 / 1000.0) }
}

/// Check if an error is transient and worth retrying.
fn is_retryable_error(err: &str) -> bool {
    let lower = err.to_lowercase();
    let retryable_markers = [
        "timeout", "timed out", "connection reset", "connection refused",
        "broken pipe", "network", "dns", "temporary", "overloaded",
        "rate limit", "429", "500", "502", "503", "504",
        "eof", "unexpected eof", "connection closed",
    ];
    let non_retryable_markers = [
        "401", "403", "402", "invalid api key", "authentication",
        "billing", "credits", "quota", "model_not_found",
        "context_length_exceeded", "invalid_request",
    ];
    // Non-retryable errors take precedence
    if non_retryable_markers.iter().any(|m| lower.contains(m)) {
        return false;
    }
    retryable_markers.iter().any(|m| lower.contains(m))
}

/// Request sent to a sub-agent.
#[derive(Debug)]
pub enum AgentRequest {
    Execute { prompt: String, result_tx: oneshot::Sender<Result<String, String>> },
    Stop,
}

/// A sub-agent is requesting input from the user.
#[derive(Debug, Clone)]
pub struct InputRequest {
    pub agent_id: String,
    pub prompt: String,
}

/// Response from the user to a sub-agent's input request.
#[derive(Debug, Clone)]
pub struct InputResponse {
    pub text: String,
}

/// Run a sub-agent in a tokio task (in-process mode).
///
/// All output goes to log file. Agent stays alive for follow-ups.
/// Sends completion notifications through `notify_tx` so the main REPL
/// can inform the coordinator agent.
///
/// The `registry` parameter is the coordinator's full tool registry (cloned).
/// This gives the sub-agent access to all tools including swarm, memory, etc.
///
/// If `router` is provided, agent-to-agent communication tools are registered
/// and the agent connects to the message router for real-time messaging.
pub async fn run_sub_agent(
    agent_id: String,
    task_id: String,
    provider: Arc<dyn Provider>,
    system: String,
    ctx: ToolContext,
    max_turns: u32,
    max_output_chars: usize,
    output_tx: OutputSender,
    notify_tx: mpsc::Sender<AgentNotification>,
    mut request_rx: mpsc::Receiver<AgentRequest>,
    mut registry: ToolRegistry,
    router: Option<Arc<MessageRouter>>,
    input_request_tx: Option<mpsc::Sender<InputRequest>>,
    mut input_response_rx: Option<mpsc::Receiver<InputResponse>>,
    display_agent_id: Option<String>,
    stream_tx: Option<mpsc::Sender<String>>,
    fallback_providers: Vec<Arc<dyn Provider>>,
) {
    // Retry configuration for transient LLM failures
    const MAX_RETRIES: u32 = 3;
    const INITIAL_BACKOFF_MS: u64 = 1000;
    let _ = output_tx.send(OutputEvent::Started {
        agent_id: agent_id.clone(),
        task_id: task_id.clone(),
    }).await;

    let log_path = log::log_dir().join(format!("{}_{}.log", agent_id, task_id));
    let log_file = std::fs::OpenOptions::new()
        .create(true).append(true).open(&log_path);
    let log = match log_file {
        Ok(f) => Arc::new(std::sync::Mutex::new(f)),
        Err(e) => {
            eprintln!("  [{}] failed to open log: {}", &agent_id[agent_id.len()-4..], e);
            return;
        }
    };

    {
        let mut f = log.lock().unwrap();
        let _ = writeln!(f, "=== Agent [{}] | Task {} ===", &agent_id[agent_id.len()-4..], &task_id[..8.min(task_id.len())]);
        let _ = f.flush();
    }

    // Always register base sub-agent tools (file/shell) so the agent
    // can work independently regardless of what the coordinator has.
    // Create a file access channel for this agent
    let (file_access_tx, _file_access_rx) = mpsc::channel(64);
    crate::tool::register_sub_agent_tools(&mut registry, &agent_id, file_access_tx);

    // Register agent-to-agent communication tools if router is available
    if let Some(ref router_arc) = router {
        crate::tool::register_agent_comm_tools(&mut registry, router_arc.clone(), &agent_id);
    }

    // Register request_input tool if input channel is available
    if let Some(ref req_tx) = input_request_tx {
        crate::tool::register_input_tool(&mut registry, req_tx.clone(), &agent_id);
    }

    // Channel for receiving user input from the display client via router
    let (display_input_tx, mut display_input_rx) = mpsc::channel::<String>(16);

    // Connect to router for real-time messages from coordinator
    let (router_tx, mut router_rx) = mpsc::channel::<AgentRequest>(16);
    let display_id_clone = display_agent_id.clone();
    if let Some(ref router_arc) = router {
        let aid = agent_id.clone();
        let addr = router_arc.addr.to_string();
        let log_clone = log.clone();
        let dtx = display_input_tx.clone();
        let disp_id = display_id_clone.clone();
        tokio::spawn(async move {
            match crate::endpoint::AgentEndpoint::connect(&addr, &aid).await {
                Ok(mut ep) => {
                    {
                        let mut f = log_clone.lock().unwrap();
                        let _ = writeln!(f, "[router] connected to {}", addr);
                    }
                    // Listen for incoming messages
                    loop {
                        match ep.read_message().await {
                            Ok(crate::router::RouterMessage::Incoming { from, content }) => {
                                // Check if this is from the display client
                                if disp_id.as_deref() == Some(from.as_str()) {
                                    // User input from display client
                                    let _ = dtx.send(content).await;
                                } else {
                                    // Task from coordinator
                                    let (result_tx, _) = oneshot::channel();
                                    let _ = router_tx.send(AgentRequest::Execute {
                                        prompt: content,
                                        result_tx,
                                    }).await;
                                }
                            }
                            Ok(crate::router::RouterMessage::Stop { .. }) => {
                                let _ = router_tx.send(AgentRequest::Stop).await;
                                break;
                            }
                            Ok(_) => {} // Ignore other messages
                            Err(e) => {
                                let mut f = log_clone.lock().unwrap();
                                let _ = writeln!(f, "[router] disconnected: {}", e);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    let mut f = log_clone.lock().unwrap();
                    let _ = writeln!(f, "[router] failed to connect: {}", e);
                }
            }
        });
    }

    let mut session = Session::new();
    // Uses the coordinator's full registry (includes swarm, memory, etc.)
    // so the sub-agent has the same capabilities as the coordinator.
    let mut is_first_turn = true;

    loop {
        // Wait for request from either mpsc channel or router
        let request = tokio::select! {
            req = request_rx.recv() => req,
            req = router_rx.recv() => req,
        };
        let request = match request {
            Some(r) => r,
            None => break, // Both channels closed
        };
        match request {
            AgentRequest::Execute { prompt, result_tx } => {
                let _ = output_tx.send(OutputEvent::Thinking { agent_id: agent_id.clone() }).await;

                {
                    let mut f = log.lock().unwrap();
                    if is_first_turn {
                        let _ = writeln!(f, "=== Prompt ===\n{}\n", prompt);
                        is_first_turn = false;
                    } else {
                        let _ = writeln!(f, "\n{}\n=== Follow-up ===\n{}\n", "─".repeat(40), prompt);
                    }
                    let _ = f.flush();
                }

                let start = Instant::now();
                session.add_user(&prompt);

                // Inline agent loop (replaces run_turn to support input requests)
                let mut turn_iter = 0u32;
                let mut llm_calls = 0u32;
                let mut tool_call_count = 0u32;
                let mut total_chars = 0usize;
                let mut final_text = String::new();
                let mut result_err: Option<String> = None;

                'turn_loop: loop {
                    turn_iter += 1;
                    if turn_iter > max_turns {
                        let mut f = log.lock().unwrap();
                        let _ = writeln!(f, "\n~ max turns ({}) reached", max_turns);
                        break;
                    }

                    // Call LLM
                    {
                        let mut f = log.lock().unwrap();
                        let _ = writeln!(f, "\n~ thinking...");
                        let _ = f.flush();
                    }

                    let api_start = Instant::now();
                    let mut current_provider = provider.clone();
                    let mut stream_result = None;
                    let mut retry_count = 0u32;
                    let mut backoff_ms = INITIAL_BACKOFF_MS;
                    let mut used_fallback = false;

                    // Retry loop with exponential backoff and model fallback
                    loop {
                        let result = current_provider.complete(
                            &session.messages, &registry.definitions(), &system,
                        ).await;

                        match result {
                            Ok(s) => {
                                stream_result = Some(s);
                                break;
                            }
                            Err(e) => {
                                let err_str = e.to_string();
                                let is_retryable = is_retryable_error(&err_str);

                                if retry_count < MAX_RETRIES && is_retryable {
                                    retry_count += 1;
                                    {
                                        let mut f = log.lock().unwrap();
                                        let _ = writeln!(f, "\n~ retry {}/{} after {}ms: {}",
                                            retry_count, MAX_RETRIES, backoff_ms, err_str);
                                    }
                                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                                    backoff_ms *= 2; // exponential backoff
                                    continue;
                                }

                                // Retries exhausted or non-retryable — try fallback providers
                                if !used_fallback && !fallback_providers.is_empty() {
                                    for (i, fb) in fallback_providers.iter().enumerate() {
                                        {
                                            let mut f = log.lock().unwrap();
                                            let _ = writeln!(f, "\n~ trying fallback provider {}...", i + 1);
                                        }
                                        let fb_result = fb.complete(
                                            &session.messages, &registry.definitions(), &system,
                                        ).await;
                                        match fb_result {
                                            Ok(s) => {
                                                current_provider = fb.clone();
                                                used_fallback = true;
                                                stream_result = Some(s);
                                                break;
                                            }
                                            Err(fb_err) => {
                                                let mut f = log.lock().unwrap();
                                                let _ = writeln!(f, "\n~ fallback {} failed: {}", i + 1, fb_err);
                                            }
                                        }
                                    }
                                    if stream_result.is_some() {
                                        break;
                                    }
                                }

                                // All retries and fallbacks exhausted
                                result_err = Some(err_str.clone());
                                write_error_log(&log, &e);
                                break;
                            }
                        }
                    }

                    let mut stream = match stream_result {
                        Some(s) => s,
                        None => break,
                    };

                    let mut text = String::new();
                    let mut tool_calls = Vec::new();
                    let mut first_delta = true;

                    while let Some(event) = stream.next().await {
                        match event {
                            Ok(StreamEvent::TextDelta(t)) => {
                                if first_delta {
                                    let elapsed = api_start.elapsed();
                                    {
                                        let mut f = log.lock().unwrap();
                                        let _ = writeln!(f, "  assistant ({})", fmt_elapsed(elapsed.as_millis() as u64));
                                    }
                                    // Send timing header to display client
                                    if let (Some(ref r), Some(ref cid)) = (&router, &display_agent_id) {
                                        let _ = r.send_to_agent(cid, &format!("  assistant ({})\n", fmt_elapsed(elapsed.as_millis() as u64))).await;
                                    }
                                    first_delta = false;
                                }
                                total_chars += t.len();
                                text.push_str(&t);
                                {
                                    let mut f = log.lock().unwrap();
                                    let _ = write!(f, "{}", t);
                                    let _ = f.flush();
                                }
                                // Stream text to display client and/or main REPL
                                if let (Some(ref r), Some(ref cid)) = (&router, &display_agent_id) {
                                    let _ = r.send_to_agent(cid, &t).await;
                                }
                                if let Some(ref tx) = stream_tx {
                                    let _ = tx.send(t.clone()).await;
                                }
                            }
                            Ok(StreamEvent::ToolCall(tc)) => {
                                if first_delta {
                                    let elapsed = api_start.elapsed();
                                    {
                                        let mut f = log.lock().unwrap();
                                        let _ = writeln!(f, "  assistant ({})", fmt_elapsed(elapsed.as_millis() as u64));
                                    }
                                    first_delta = false;
                                }
                                tool_calls.push(tc);
                            }
                            Ok(StreamEvent::End) => break,
                            Ok(StreamEvent::Raw(_)) => {}
                            Err(e) => {
                                result_err = Some(e.to_string());
                                break;
                            }
                        }
                    }

                    llm_calls += 1;

                    // No tool calls → turn is done
                    if tool_calls.is_empty() {
                        if !text.is_empty() {
                            session.add_assistant(&text);
                        }
                        final_text = text;
                        break;
                    }

                    // Has tool calls → execute them
                    session.add_assistant_with_tools(&text, &tool_calls);

                    {
                        let mut f = log.lock().unwrap();
                        let names: Vec<&str> = tool_calls.iter().map(|tc| tc.name.as_str()).collect();
                        let _ = writeln!(f, "\n  tools: {}", names.join(" · "));
                    }

                    // Execute all tool calls in parallel (same as main agent)
                    let tool_results: Vec<(usize, flint_types::ToolOutput, std::time::Duration)> = {
                        use futures::future::join_all;
                        let futs: Vec<_> = tool_calls.iter().enumerate().map(|(i, tc)| {
                            let reg = registry.clone();
                            let c = ctx.clone();
                            let name = tc.name.clone();
                            let input = tc.input.clone();
                            async move {
                                let tool_start = Instant::now();
                                let output = tokio::time::timeout(
                                    std::time::Duration::from_secs(600),
                                    reg.execute(&name, input, &c),
                                ).await;
                                let elapsed = tool_start.elapsed();
                                let output = match output {
                                    Ok(result) => result.unwrap_or_else(|e| {
                                        flint_types::ToolOutput::error(format!("tool error: {}", e))
                                    }),
                                    Err(_) => flint_types::ToolOutput::error(format!(
                                        "tool '{}' timed out after 600 seconds", name
                                    )),
                                };
                                (i, output, elapsed)
                            }
                        }).collect();
                        join_all(futs).await
                    };

                    // Process results and check for input requests
                    let mut input_requested = false;
                    let mut input_prompt = String::new();

                    for (i, output, tool_elapsed) in tool_results {
                        tool_call_count += 1;

                        // Truncate if needed
                        let output = if output.text.len() > max_output_chars {
                            let truncated = format!(
                                "{}\n\n[truncated — output was {} chars, limit is {}]",
                                &output.text[..max_output_chars], output.text.len(), max_output_chars
                            );
                            flint_types::ToolOutput { text: truncated, is_error: output.is_error }
                        } else {
                            output
                        };

                        // Check if this is an input request
                        if output.text.starts_with(INPUT_REQUESTED_PREFIX) {
                            input_requested = true;
                            input_prompt = output.text
                                .strip_prefix(INPUT_REQUESTED_PREFIX)
                                .unwrap_or(&output.text)
                                .trim_end_matches(']')
                                .to_string();
                        }

                        {
                            let mut f = log.lock().unwrap();
                            let icon = if output.is_error { "x" } else { "+" };
                            let preview: String = output.text.chars().take(200).collect();
                            let _ = writeln!(f, "  {} {} ({})", icon, preview,
                                fmt_elapsed(tool_elapsed.as_millis() as u64));
                        }
                        // Send tool result to display client and/or main REPL
                        {
                            let preview: String = output.text.chars().take(200).collect();
                            let tool_msg = format!(
                                "\n[TOOL:{} {} ({})]\n", tool_calls[i].name, preview,
                                fmt_elapsed(tool_elapsed.as_millis() as u64)
                            );
                            if let (Some(ref r), Some(ref cid)) = (&router, &display_agent_id) {
                                let _ = r.send_to_agent(cid, &tool_msg).await;
                            }
                            if let Some(ref tx) = stream_tx {
                                let _ = tx.send(tool_msg).await;
                            }
                        }

                        session.add_tool_result(&tool_calls[i].id, &output);
                    }

                    // If input was requested, wait for the user's response
                    if input_requested {
                        {
                            let mut f = log.lock().unwrap();
                            let _ = writeln!(f, "\n  [input requested] {}", input_prompt);
                            let _ = f.flush();
                        }

                        // Send input request to display client, main REPL, and stream
                        if let (Some(ref r), Some(ref cid)) = (&router, &display_agent_id) {
                            let _ = r.send_to_agent(cid, &format!(
                                "[INPUT_REQUESTED:{}]", input_prompt
                            )).await;
                        }
                        if let Some(ref tx) = stream_tx {
                            let _ = tx.send(format!("\n[INPUT_REQUESTED:{}]", input_prompt)).await;
                        }

                        // Also send to REPL handler (if connected)
                        let req = InputRequest {
                            agent_id: agent_id.clone(),
                            prompt: input_prompt.clone(),
                        };
                        if let Some(ref req_tx) = input_request_tx {
                            let _ = req_tx.send(req).await;
                        }

                        // Wait for the user's response — from display client or REPL
                        let user_input = tokio::select! {
                            // From display client via router
                            input = display_input_rx.recv() => input,
                            // From REPL handler via channel
                            resp = async {
                                if let Some(ref mut rx) = input_response_rx {
                                    rx.recv().await.map(|r| r.text)
                                } else {
                                    futures::future::pending().await
                                }
                            } => resp,
                        };

                        match user_input {
                            Some(text) => {
                                {
                                    let mut f = log.lock().unwrap();
                                    let _ = writeln!(f, "  [user input] {}", text);
                                }
                                session.add_user(&text);
                            }
                            None => {
                                let mut f = log.lock().unwrap();
                                let _ = writeln!(f, "  [input channel closed]");
                                break 'turn_loop;
                            }
                        }

                        // Continue the turn loop — LLM will see the user's response
                        continue 'turn_loop;
                    }

                    // No input requested — loop back so LLM sees tool results
                    // and can produce final text or call more tools
                    continue 'turn_loop;
                }

                let elapsed_ms = start.elapsed().as_millis() as u64;

                let final_result = if let Some(err) = result_err {
                    write_error_log(&log, &anyhow::anyhow!("{}", err));
                    let _ = output_tx.send(OutputEvent::Failed {
                        agent_id: agent_id.clone(),
                        error: err.clone(),
                    }).await;
                    Err(err)
                } else {
                    write_done_log(&log, &final_text, llm_calls, tool_call_count, total_chars, elapsed_ms);
                    let _ = output_tx.send(OutputEvent::Done {
                        agent_id: agent_id.clone(),
                        elapsed: fmt_elapsed(elapsed_ms),
                        chars: total_chars,
                    }).await;
                    Ok(final_text)
                };

                // Send [DONE] to display client and stream
                if let (Some(ref r), Some(ref cid)) = (&router, &display_agent_id) {
                    let _ = r.send_to_agent(cid, "[DONE]").await;
                }
                if let Some(ref tx) = stream_tx {
                    let _ = tx.send("[DONE]".to_string()).await;
                }

                // Send notification to the main REPL
                let notification = AgentNotification {
                    agent_id: agent_id.clone(),
                    task_id: task_id.clone(),
                    result: final_result.clone(),
                };
                let _ = notify_tx.send(notification).await;

                // Send result to the oneshot channel
                let _ = result_tx.send(final_result);
            }
            AgentRequest::Stop => {
                let _ = output_tx.send(OutputEvent::Failed {
                    agent_id: agent_id.clone(),
                    error: "stopped".to_string(),
                }).await;
                break;
            }
        }
    }
}

/// Spawn a display client in a new terminal window.
///
/// The display client is a thin terminal that connects to the swarm's
/// MessageRouter via TCP. It shows the agent's output in real-time and
/// forwards user input to the agent — jcode-style client-server architecture.
pub fn spawn_display_client(
    display_client_id: &str,
    agent_id: &str,
    working_dir: &std::path::Path,
    router_addr: Option<&str>,
) {
    let flint_exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "flint".to_string());

    let dir_str = working_dir.to_string_lossy().to_string();
    let title = format!("Agent [{}]", &agent_id[agent_id.len()-4..]);

    let router_flag = router_addr
        .map(|addr| format!(" --router-addr {}", addr))
        .unwrap_or_default();

    // Launch flint in display client mode in a new CMD window
    #[cfg(target_os = "windows")]
    {
        let bat_path = log::log_dir().join(format!("{}_display.bat", display_client_id));
        let bat_content = format!(
            "@echo off\r\n\
             chcp 65001 >nul\r\n\
             title {title}\r\n\
             \"{flint}\" --display --dir \"{dir}\" --agent-id \"{aid}\"{router_flag}\r\n\
             echo.\r\n\
             echo === Display client exited. Press any key to close ===\r\n\
             pause >nul",
            title = title,
            flint = flint_exe,
            dir = dir_str,
            aid = display_client_id,
            router_flag = router_flag,
        );
        let _ = std::fs::write(&bat_path, &bat_content);
        let bat_str = bat_path.to_string_lossy().to_string();
        let cmd_exe = std::env::var("COMSPEC").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".to_string());
        let _ = std::process::Command::new(&cmd_exe)
            .args(["/C", "start", &title, &cmd_exe, "/K", &bat_str])
            .spawn();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let cmd = format!(
            "{} --display --dir '{}' --agent-id '{}'{}",
            flint_exe, dir_str, display_client_id, router_flag,
        );
        if std::env::var("TMUX").is_ok() {
            let _ = std::process::Command::new("tmux")
                .args(["split-window", "-h", "-l", "40%", &cmd]).status();
        } else {
            for term in &["xterm", "gnome-terminal", "konsole"] {
                let r = match *term {
                    "gnome-terminal" => std::process::Command::new(term)
                        .args(["--title", &title, "--", "bash", "-c", &cmd]).spawn(),
                    _ => std::process::Command::new(term)
                        .args(["-e", &format!("bash -c '{}'", cmd)]).spawn(),
                };
                if r.is_ok() { break; }
            }
        }
    }
}

/// Legacy: Spawn an external interactive sub-agent in a new terminal.
/// Deprecated — use spawn_display_client instead.
pub fn spawn_interactive_agent(
    agent_id: &str,
    task_id: &str,
    prompt: &str,
    working_dir: &std::path::Path,
    router_addr: Option<&str>,
) {
    // Write task context file
    let ctx_path = log::log_dir().join(format!("{}_{}.ctx.md", agent_id, task_id));
    let _ = std::fs::write(&ctx_path, format!(
        "# Swarm Task [{}]\n\n{}\n\n---\nComplete the task above in this REPL.\n",
        &agent_id[agent_id.len()-4..], prompt
    ));

    // Write result placeholder path
    let result_path = log::log_dir().join(format!("{}_{}.result.md", agent_id, task_id));
    let result_path_str = result_path.to_string_lossy().to_string();

    // Build system prompt for the sub-agent (inline, not file)
    let sub_system = format!(
        "You are a swarm sub-agent [{}]. Your task:\n\n{}\n\n\
         Work independently. Use tools as needed.\n\
         When you have completed the task, write your final summary/result to this file:\n\
         {}\n\
         Use the `write` tool to save your result there.\n\
         After writing the result, you can continue to interact with the user.",
        &agent_id[agent_id.len()-4..], prompt, result_path_str
    );

    let flint_exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "flint".to_string());

    let dir_str = working_dir.to_string_lossy().to_string();
    let title = format!("Agent [{}]", &agent_id[agent_id.len()-4..]);

    // Write system prompt to file (avoid escaping issues in batch)
    let sys_path = log::log_dir().join(format!("{}_{}.system.txt", agent_id, task_id));
    let _ = std::fs::write(&sys_path, &sub_system);
    let sys_path_str = sys_path.to_string_lossy().to_string();

    // Write initial message to file (avoids shell escaping issues)
    let msg_path = log::log_dir().join(format!("{}_{}.msg.txt", agent_id, task_id));
    let _ = std::fs::write(&msg_path, prompt);
    let msg_path_str = msg_path.to_string_lossy().to_string();

    // Create message file for coordinator → sub-agent communication.
    // The coordinator writes follow-up messages here; the sub-agent's REPL
    // checks this file before each turn and injects messages as context.
    let comm_path = log::log_dir().join(format!("{}_{}.comm.txt", agent_id, task_id));
    let _ = std::fs::write(&comm_path, "");
    let _comm_path_str = comm_path.to_string_lossy().to_string();

    // Build router flag if available
    let router_flag = router_addr
        .map(|addr| format!(" --router-addr {}", addr))
        .unwrap_or_default();

    // Launch flint REPL in a new console window.
    // On Windows, use `cmd /C start "title" cmd /K "flint ..."` which
    // opens a new CMD window with its own console (stdin/stdout).
    // The new window runs flint directly — no batch script needed.
    #[cfg(target_os = "windows")]
    {
        let flint_cmd = format!(
            "\"{}\" --dir \"{}\" --system-file \"{}\" --initial-message-file \"{}\" --agent-id \"{}\"{}",
            flint_exe, dir_str, sys_path_str, msg_path_str, agent_id, router_flag,
        );
        let cmd_exe = std::env::var("COMSPEC").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".to_string());
        let _ = std::process::Command::new(&cmd_exe)
            .args(["/C", "start", &title, &cmd_exe, "/K", &flint_cmd])
            .spawn();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let cmd = format!(
            "{} --dir '{}' --system-file '{}' --initial-message-file '{}' --agent-id '{}'{}",
            flint_exe, dir_str, sys_path_str, msg_path_str, agent_id, router_flag
        );
        if std::env::var("TMUX").is_ok() {
            let _ = std::process::Command::new("tmux")
                .args(["split-window", "-h", "-l", "40%", &cmd]).status();
        } else {
            for term in &["xterm", "gnome-terminal", "konsole"] {
                let r = match *term {
                    "gnome-terminal" => std::process::Command::new(term)
                        .args(["--title", &title, "--", "bash", "-c", &cmd]).spawn(),
                    _ => std::process::Command::new(term)
                        .args(["-e", &format!("bash -c '{}'", cmd)]).spawn(),
                };
                if r.is_ok() { break; }
            }
        }
    }
}

/// Send a message to an interactive sub-agent via its communication file.
/// The sub-agent's REPL checks this file before each turn.
pub fn send_message_to_interactive(agent_id: &str, task_id: &str, message: &str) -> anyhow::Result<()> {
    let comm_path = log::log_dir().join(format!("{}_{}.comm.txt", agent_id, task_id));
    if !comm_path.exists() {
        return Err(anyhow::anyhow!("communication file not found for agent {}", agent_id));
    }
    // Append the message (with newline separator) to the file
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true).append(true).open(&comm_path)?;
    writeln!(file, "{}", message)?;
    Ok(())
}

/// Get the communication file path for an interactive agent.
pub fn interactive_comm_path(agent_id: &str, task_id: &str) -> std::path::PathBuf {
    log::log_dir().join(format!("{}_{}.comm.txt", agent_id, task_id))
}

/// Spawn a full sub-agent REPL in a new terminal (方案 A).
///
/// The sub-agent runs as an independent `flint` process with:
/// - Inherited system prompt and optional conversation history
/// - Its own Session, LLM calls, and tool execution
/// - Direct user interaction via its own stdin/stdout
/// - Communication with coordinator via TCP MessageRouter
///
/// Returns the path to the SpawnContext JSON file.
pub fn spawn_terminal_agent(
    agent_id: &str,
    task_id: &str,
    prompt: &str,
    system_prompt: &str,
    conversation_history: Option<Vec<flint_types::Message>>,
    core_memory: &str,
    working_dir: &std::path::Path,
    router_addr: &str,
    full_context: bool,
    model: Option<String>,
) -> anyhow::Result<std::path::PathBuf> {
    use crate::types::SpawnContext;

    let ctx = SpawnContext {
        agent_id: agent_id.to_string(),
        task_id: task_id.to_string(),
        system_prompt: system_prompt.to_string(),
        conversation_history: if full_context { conversation_history } else { None },
        core_memory: core_memory.to_string(),
        router_addr: router_addr.to_string(),
        working_dir: working_dir.to_path_buf(),
        initial_prompt: prompt.to_string(),
        model,
    };

    // Serialize to JSON file in the log directory
    let ctx_path = log::log_dir().join(format!("{}_{}.spawn.json", agent_id, task_id));
    let json = serde_json::to_string_pretty(&ctx)?;
    std::fs::write(&ctx_path, &json)?;

    let flint_exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "flint".to_string());

    let dir_str = working_dir.to_string_lossy().to_string();
    let ctx_path_str = ctx_path.to_string_lossy().to_string();
    let title = format!("Agent [{}]", &agent_id[agent_id.len()-4..]);

    // Launch flint REPL in a new terminal window with spawn context
    #[cfg(target_os = "windows")]
    {
        let bat_path = log::log_dir().join(format!("{}_terminal.bat", agent_id));
        let bat_content = format!(
            "@echo off\r\n\
             chcp 65001 >nul\r\n\
             title {title}\r\n\
             \"{flint}\" --dir \"{dir}\" --spawn-context \"{ctx_path}\"\r\n\
             echo.\r\n\
             echo === Sub-agent exited. Press any key to close ===\r\n\
             pause >nul",
            title = title,
            flint = flint_exe,
            dir = dir_str,
            ctx_path = ctx_path_str,
        );
        let _ = std::fs::write(&bat_path, &bat_content);
        let bat_str = bat_path.to_string_lossy().to_string();
        // Use cmd.exe explicitly to avoid Windows Terminal / PowerShell interception
        let cmd_exe = std::env::var("COMSPEC").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".to_string());
        let _ = std::process::Command::new(&cmd_exe)
            .args(["/C", "start", &title, &cmd_exe, "/K", &bat_str])
            .spawn();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let cmd = format!(
            "{} --dir '{}' --spawn-context '{}'",
            flint_exe, dir_str, ctx_path_str,
        );
        if std::env::var("TMUX").is_ok() {
            let _ = std::process::Command::new("tmux")
                .args(["split-window", "-h", "-l", "40%", &cmd]).status();
        } else {
            for term in &["xterm", "gnome-terminal", "konsole"] {
                let r = match *term {
                    "gnome-terminal" => std::process::Command::new(term)
                        .args(["--title", &title, "--", "bash", "-c", &cmd]).spawn(),
                    _ => std::process::Command::new(term)
                        .args(["-e", &format!("bash -c '{}'", cmd)]).spawn(),
                };
                if r.is_ok() { break; }
            }
        }
    }

    Ok(ctx_path)
}

fn _write_to_log(log: &Arc<std::sync::Mutex<std::fs::File>>, _agent_id: &str, event: &AgentEvent) {
    let mut f = log.lock().unwrap();
    match event {
        AgentEvent::Thinking => { let _ = writeln!(f, "\n~ thinking..."); }
        AgentEvent::TextDelta(text) => { let _ = write!(f, "{}", text); }
        AgentEvent::ToolCallStart { name, input_preview } => {
            let _ = writeln!(f, "\n* {} {}", name, input_preview);
        }
        AgentEvent::ToolCallEnd { name, success, preview, elapsed_ms } => {
            let icon = if *success { "+" } else { "x" };
            let _ = writeln!(f, "  {} {} {} ({})", icon, name, preview,
                if *elapsed_ms < 1000 { format!("{}ms", elapsed_ms) } else { format!("{:.1}s", *elapsed_ms as f64 / 1000.0) });
        }
        AgentEvent::TurnComplete { tool_calls, chars, elapsed_ms, .. } => {
            let _ = writeln!(f, "\n-- turn complete · {} · {} chars · {} tools --",
                if *elapsed_ms < 1000 { format!("{}ms", elapsed_ms) } else { format!("{:.1}s", *elapsed_ms as f64 / 1000.0) },
                chars, tool_calls);
        }
    }
    let _ = f.flush();
}

fn write_done_log(log: &Arc<std::sync::Mutex<std::fs::File>>, text: &str, llm_calls: u32, tool_calls: u32, chars: usize, elapsed_ms: u64) {
    let mut f = log.lock().unwrap();
    let _ = writeln!(f, "\n\n=== Result ===\n{}", text);
    let _ = writeln!(f, "\n=== Done ({} calls, {} tools, {} chars, {}) ===",
        llm_calls, tool_calls, chars, fmt_elapsed(elapsed_ms));
    let _ = f.flush();
}

fn write_error_log(log: &Arc<std::sync::Mutex<std::fs::File>>, e: &anyhow::Error) {
    let mut f = log.lock().unwrap();
    let _ = writeln!(f, "\n=== Error: {} ===", e);
    let _ = f.flush();
}
