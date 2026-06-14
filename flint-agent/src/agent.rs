//! The core agent loop.
//!
//! ```text
//! user message → LLM → if tool_calls → execute → loop
//!                    → if text only  → done
//! ```

use anyhow::Result;
use flint_provider::Provider;
use flint_types::StreamEvent;
use futures::StreamExt;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::session::Session;
use crate::tool::{ToolContext, ToolRegistry};
use std::time::Duration;

/// Default timeout for individual tool execution (120 seconds).
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(120);

// ── ANSI color helpers ────────────────────────────────────────────────────

fn dim(s: &str) -> String {
    format!("\x1b[90m{}\x1b[0m", s)
}

fn _green(s: &str) -> String {
    format!("\x1b[32m{}\x1b[0m", s)
}

fn red(s: &str) -> String {
    format!("\x1b[31m{}\x1b[0m", s)
}

fn yellow(s: &str) -> String {
    format!("\x1b[33m{}\x1b[0m", s)
}

fn _bold(s: &str) -> String {
    format!("\x1b[1m{}\x1b[0m", s)
}

fn cyan(s: &str) -> String {
    format!("\x1b[36m{}\x1b[0m", s)
}

fn format_elapsed(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 1.0 {
        format!("{:.0}ms", elapsed.as_millis())
    } else if secs < 60.0 {
        format!("{:.1}s", secs)
    } else {
        format!("{}m{:.0}s", secs as u64 / 60, secs % 60.0)
    }
}

/// Strip ANSI escape sequences and normalize line endings for safe terminal display.
fn sanitize_preview(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip ANSI escape sequence: ESC [ ... letter
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() || next == 'm' {
                        break;
                    }
                }
            }
        } else if c == '\r' {
            // Normalize \r\n → \n, lone \r → \n
            if chars.peek() != Some(&'\n') {
                out.push('\n');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Statistics collected during a turn.
#[derive(Debug, Clone, Default)]
pub struct TurnStats {
    pub llm_calls: u32,
    pub tool_calls: u32,
    pub total_chars: usize,
}

/// Run a single agent turn: send messages to the provider, stream the response,
/// execute any tool calls, and loop until the LLM produces a final text response.
///
/// When `silent` is true, suppresses all terminal output (used by background tasks
/// like compaction/extraction). Sub-agents should use `silent=true` with a `callback`
/// for real-time event forwarding.
///
/// `callback` receives structured events for real-time observation. If the callback
/// returns `true`, the default terminal output is also shown; if `false`, it's suppressed.
///
/// Returns the assistant's final text response and turn statistics.
pub async fn run_turn(
    provider: &dyn Provider,
    session: &mut Session,
    registry: &ToolRegistry,
    system: &str,
    ctx: &ToolContext,
    max_turns: u32,
    cancel: Option<Arc<AtomicBool>>,
    max_output_chars: usize,
    silent: bool,
    callback: Option<&crate::EventCallback>,
    render_fn: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<(String, TurnStats)> {
    let turn_start = Instant::now();
    let mut turn_iter = 0u32;
    let mut stats = TurnStats::default();
    const MAX_CONSECUTIVE_ERRORS: u32 = 3;

    loop {
        turn_iter += 1;

        // Check max_turns limit
        if turn_iter > max_turns {
            if !silent {
                eprintln!(
                    "{}",
                    yellow(&format!(
                        "  ── max turns ({}) reached, stopping ──",
                        max_turns
                    ))
                );
            }
            break;
        }

        // Check cancellation
        if cancel.as_ref().map_or(false, |f| f.load(Ordering::Relaxed)) {
            if !silent {
                eprintln!("{}", yellow("  ── interrupted by user ──"));
            }
            break;
        }

        // Emit thinking event
        let mut print_this = !silent;
        if let Some(cb) = &callback {
            if !cb(&crate::AgentEvent::Thinking) {
                print_this = false;
            }
        }
        if print_this {
            print!("\r\x1b[K{} {}", cyan("~"), dim("Thinking..."));
            std::io::stdout().flush()?;
        }

        let api_start = Instant::now();
        let mut stream = provider
            .complete(&session.messages, &registry.definitions(), system)
            .await?;

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut first_delta = true;
        let mut token_count: usize = 0;
        // Line buffer for streaming markdown rendering
        let mut line_buffer = String::new();

        while let Some(event) = stream.next().await {
            // Check cancellation during streaming — allows interrupting long LLM responses
            if cancel.as_ref().map_or(false, |f| f.load(Ordering::Relaxed)) {
                if !silent {
                    eprintln!("\n{}", yellow("  ── interrupted by user ──"));
                }
                break;
            }
            match event? {
                StreamEvent::TextDelta(t) => {
                    if first_delta {
                        let elapsed = api_start.elapsed();
                        let mut print_delta = !silent;
                        if let Some(cb) = &callback {
                            if !cb(&crate::AgentEvent::Thinking) { print_delta = false; }
                        }
                        if print_delta {
                            print!("\r\x1b[K");
                            eprint!("\r\x1b[K");
                            eprintln!("{}", dim(&format!("  assistant ({})", format_elapsed(elapsed))));
                        }
                        first_delta = false;
                    }
                    token_count += t.len();
                    text.push_str(&t);
                    let mut print_text = !silent;
                    if let Some(cb) = &callback {
                        if !cb(&crate::AgentEvent::TextDelta(t.clone())) { print_text = false; }
                    }
                    if print_text {
                        // Buffer text and render complete lines
                        line_buffer.push_str(&t);
                        while let Some(newline_pos) = line_buffer.find('\n') {
                            let line = &line_buffer[..newline_pos];
                            if let Some(ref render_fn) = render_fn {
                                render_fn(line);
                                println!();
                            } else {
                                println!("{}", line);
                            }
                            line_buffer = line_buffer[newline_pos + 1..].to_string();
                        }
                    }
                }
                StreamEvent::ToolCall(tc) => {
                    if first_delta {
                        let elapsed = api_start.elapsed();
                        let mut print_tc = !silent;
                        if let Some(cb) = &callback {
                            if !cb(&crate::AgentEvent::Thinking) { print_tc = false; }
                        }
                        if print_tc {
                            print!("\r\x1b[K");
                            eprint!("\r\x1b[K");
                            eprintln!("{}", dim(&format!("  assistant ({})", format_elapsed(elapsed))));
                        }
                        first_delta = false;
                    }
                    tool_calls.push(tc);
                }
                StreamEvent::End => break,
                StreamEvent::Raw(_) => {}
            }
        }

        // Flush remaining line buffer
        if !silent && !line_buffer.is_empty() {
            if let Some(ref render_fn) = render_fn {
                render_fn(&line_buffer);
            } else {
                print!("{}", line_buffer);
            }
            line_buffer.clear();
        }

        // If cancelled during streaming, return what we have so far
        if cancel.as_ref().map_or(false, |f| f.load(Ordering::Relaxed)) {
            if !text.is_empty() {
                session.add_assistant(&text);
            }
            stats.llm_calls = turn_iter;
            stats.total_chars = token_count;
            return Ok((text, stats));
        }

        // No tool calls → turn is done
        if tool_calls.is_empty() {
            if !text.is_empty() {
                session.add_assistant(&text);
            }
            stats.llm_calls = turn_iter;
            stats.total_chars = token_count;
            let total_elapsed = turn_start.elapsed();
            if let Some(cb) = &callback {
                cb(&crate::AgentEvent::TurnComplete {
                    text: text.clone(),
                    llm_calls: stats.llm_calls,
                    tool_calls: stats.tool_calls,
                    chars: token_count,
                    elapsed_ms: total_elapsed.as_millis() as u64,
                });
            }
            if !silent {
                println!();
                if text.is_empty() {
                    eprintln!(
                        "{}",
                        yellow(&format!(
                            "  ── turn complete · {} · no response (empty) ──",
                            format_elapsed(total_elapsed)
                        ))
                    );
                } else {
                    eprintln!(
                        "{}",
                        dim(&format!(
                            "  ── turn complete · {} · {} chars · {} tool calls ──",
                            format_elapsed(total_elapsed),
                            token_count,
                            stats.tool_calls
                        ))
                    );
                }
                println!();
            }
            return Ok((text, stats));
        }

        // Has tool calls → execute them and loop
        session.add_assistant_with_tools(&text, &tool_calls);

        if !silent {
            // Show tool calls header
            let tool_names: Vec<&str> = tool_calls.iter().map(|tc| tc.name.as_str()).collect();
            if !text.is_empty() {
                println!();
            }
            eprintln!(
                "{}",
                dim(&format!(
                    "  tools: {}",
                    tool_names.join(" · ")
                ))
            );

            // Display tool call previews
            for tc in tool_calls.iter() {
                print!("  {} {}", dim("*"), dim(&tc.name));
                if let Some(input_str) = tc.input.as_str() {
                    let preview: String = input_str.chars().take(80).collect();
                    if !preview.is_empty() {
                        print!(" {}", dim(&preview));
                    }
                } else if let Some(obj) = tc.input.as_object() {
                    if let Some((k, v)) = obj.iter().next() {
                        let val_str = match v {
                            serde_json::Value::String(s) => {
                                let preview: String = s.chars().take(60).collect();
                                if preview.len() < s.len() { format!("{}...", preview) } else { preview }
                            }
                            other => {
                                let s = other.to_string();
                                let preview: String = s.chars().take(60).collect();
                                if preview.len() < s.len() { format!("{}...", preview) } else { preview }
                            }
                        };
                        print!(" {}", dim(&format!("{}: {}", k, val_str)));
                    }
                }
                println!();
            }
            std::io::stdout().flush()?;
        }

        // Execute tool calls — parallel when multiple
        let tool_results: Vec<(usize, flint_types::ToolOutput, std::time::Duration)> = {
            use futures::future::join_all;
            let futs: Vec<_> = tool_calls.iter().enumerate().map(|(i, tc)| {
                let tc_name = &tc.name;
                let tc_input = &tc.input;
                // Use per-tool timeout if defined, otherwise default
                let tool_timeout = registry.tool_timeout(tc_name)
                    .unwrap_or(DEFAULT_TOOL_TIMEOUT);
                async move {
                    let tool_start = Instant::now();
                    let output = tokio::time::timeout(
                        tool_timeout,
                        registry.execute(tc_name, tc_input.clone(), ctx),
                    ).await;
                    let elapsed = tool_start.elapsed();
                    let output = match output {
                        Ok(result) => result.unwrap_or_else(|e| {
                            flint_types::ToolOutput::error(format!("tool error: {}", e))
                        }),
                        Err(_) => flint_types::ToolOutput::error(format!(
                            "tool '{}' timed out after {} seconds",
                            tc_name,
                            tool_timeout.as_secs()
                        )),
                    };
                    (i, output, elapsed)
                }
            }).collect();
            join_all(futs).await
        };

        // Display results and add to session in order
        for (i, output, tool_elapsed) in tool_results {
            stats.tool_calls += 1;

            // Truncate tool output if it exceeds max_output_chars
            let output = if output.text.len() > max_output_chars {
                // Safe truncation at char boundary to avoid UTF-8 panic
                let truncate_at = output.text.char_indices()
                    .nth(max_output_chars)
                    .map(|(i, _)| i)
                    .unwrap_or(output.text.len());
                let truncated_text = format!(
                    "{}\n\n[truncated — output was {} chars, limit is {}]",
                    &output.text[..truncate_at],
                    output.text.len(),
                    max_output_chars
                );
                flint_types::ToolOutput {
                    text: truncated_text,
                    is_error: output.is_error,
                }
            } else {
                output
            };

            let sanitized = sanitize_preview(&output.text);
            let preview: String = sanitized.chars().take(200).collect();
            let truncated = if sanitized.len() > 200 { "..." } else { "" };
            let mut print_result = !silent;
            if let Some(cb) = &callback {
                if !cb(&crate::AgentEvent::ToolCallEnd {
                    name: tool_calls[i].name.clone(),
                    success: !output.is_error,
                    preview: preview.clone(),
                    elapsed_ms: tool_elapsed.as_millis() as u64,
                }) {
                    print_result = false;
                }
            }
            if print_result {
                if output.is_error {
                    println!("  {} {} {}", red("x"), dim(&preview), dim(&format!("({})", format_elapsed(tool_elapsed))));
                } else {
                    println!("  {} {}{} {}", dim("+"), dim(&preview), dim(truncated), dim(&format!("({})", format_elapsed(tool_elapsed))));
                }
            }

            // Circuit breaker: track consecutive same-tool errors (persists across turns)
            let output = if output.is_error {
                let tool_name = &tool_calls[i].name;
                if session.circuit_breaker_last_tool.as_deref() == Some(tool_name) {
                    session.circuit_breaker_count += 1;
                } else {
                    session.circuit_breaker_last_tool = Some(tool_name.clone());
                    session.circuit_breaker_count = 1;
                }
                if session.circuit_breaker_count >= MAX_CONSECUTIVE_ERRORS {
                    let warning = format!(
                        "\n\n⚠ CIRCUIT BREAKER: This tool has failed {} consecutive times (across turns). \
                        STOP retrying the same approach. Instead:\n\
                        1. Use `read` to get the exact current file content before using `edit`.\n\
                        2. If the task is complex, tell the user to run `/compact` to reduce context.\n\
                        3. Try a completely different strategy.",
                        session.circuit_breaker_count
                    );
                    flint_types::ToolOutput {
                        text: format!("{}{}", output.text, warning),
                        is_error: output.is_error,
                    }
                } else {
                    output
                }
            } else {
                // Reset on success
                session.circuit_breaker_count = 0;
                session.circuit_breaker_last_tool = None;
                output
            };

            session.add_tool_result(&tool_calls[i].id, &output);
        }

        // If cancelled during tool execution, break the outer loop
        if cancel.as_ref().map_or(false, |f| f.load(Ordering::Relaxed)) {
            eprintln!("{}", yellow("  ── interrupted by user ──"));
            break;
        }

        // Separator between tool execution and next LLM call
        if !silent {
            let so_far = turn_start.elapsed();
            println!(
                "{}",
                dim(&format!(
                    "  ── turn {} · tool {}/{} · {} elapsed ──",
                    turn_iter,
                    tool_calls.len(),
                    tool_calls.len(),
                    format_elapsed(so_far)
                ))
            );
            println!();
        }
    }

    // Reached via break (max_turns or cancel) — return what we have
    stats.llm_calls = turn_iter;
    Ok((String::new(), stats))
}
