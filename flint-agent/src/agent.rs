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
use std::time::Instant;

use crate::session::Session;
use crate::tool::{ToolContext, ToolRegistry};

// ── ANSI color helpers ────────────────────────────────────────────────────

fn dim(s: &str) -> String {
    format!("\x1b[90m{}\x1b[0m", s)
}

fn green(s: &str) -> String {
    format!("\x1b[32m{}\x1b[0m", s)
}

fn red(s: &str) -> String {
    format!("\x1b[31m{}\x1b[0m", s)
}

fn yellow(s: &str) -> String {
    format!("\x1b[33m{}\x1b[0m", s)
}

fn bold(s: &str) -> String {
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

/// Run a single agent turn: send messages to the provider, stream the response,
/// execute any tool calls, and loop until the LLM produces a final text response.
///
/// Returns the assistant's final text response.
pub async fn run_turn(
    provider: &dyn Provider,
    session: &mut Session,
    registry: &ToolRegistry,
    system: &str,
    ctx: &ToolContext,
) -> Result<String> {
    let turn_start = Instant::now();
    let mut turn_iter = 0u32;

    loop {
        turn_iter += 1;

        // Show thinking indicator
        print!("\r\x1b[K{} {}", cyan("⟳"), dim("Thinking..."));
        std::io::stdout().flush()?;

        let api_start = Instant::now();
        let mut stream = provider
            .complete(&session.messages, &registry.definitions(), system)
            .await?;

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut first_delta = true;
        let mut token_count: usize = 0;

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::TextDelta(t) => {
                    if first_delta {
                        let elapsed = api_start.elapsed();
                        print!("\r\x1b[K");
                        eprint!("\r\x1b[K");
                        // Show a subtle header before assistant response
                        eprintln!(
                            "{}",
                            dim(&format!("  assistant ({})", format_elapsed(elapsed)))
                        );
                        first_delta = false;
                    }
                    token_count += t.len();
                    text.push_str(&t);
                    print!("{}", t);
                }
                StreamEvent::ToolCall(tc) => {
                    if first_delta {
                        let elapsed = api_start.elapsed();
                        print!("\r\x1b[K");
                        eprint!("\r\x1b[K");
                        eprintln!(
                            "{}",
                            dim(&format!("  assistant ({})", format_elapsed(elapsed)))
                        );
                        first_delta = false;
                    }
                    tool_calls.push(tc);
                }
                StreamEvent::End => break,
                StreamEvent::Raw(_) => {}
            }
        }

        // No tool calls → turn is done
        if tool_calls.is_empty() {
            session.add_assistant(&text);
            let total_elapsed = turn_start.elapsed();
            println!();
            // Turn footer with stats
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
                        "  ── turn complete · {} · {} chars ──",
                        format_elapsed(total_elapsed),
                        token_count
                    ))
                );
            }
            println!();
            return Ok(text);
        }

        // Has tool calls → execute them and loop
        session.add_assistant_with_tools(&text, &tool_calls);

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

        for tc in tool_calls.iter() {
            let tool_start = Instant::now();
            print!(
                "  {} {}",
                yellow("⚙"),
                bold(&tc.name)
            );
            // Show input preview for key tools
            if let Some(input_str) = tc.input.as_str() {
                let preview: String = input_str.chars().take(80).collect();
                if !preview.is_empty() {
                    print!(" {}", dim(&preview));
                }
            } else if let Some(obj) = tc.input.as_object() {
                // Show first key-value pair as preview
                if let Some((k, v)) = obj.iter().next() {
                    let val_str = match v {
                        serde_json::Value::String(s) => {
                            let preview: String = s.chars().take(60).collect();
                            if preview.len() < s.len() {
                                format!("{}…", preview)
                            } else {
                                preview
                            }
                        }
                        other => {
                            let s = other.to_string();
                            let preview: String = s.chars().take(60).collect();
                            if preview.len() < s.len() {
                                format!("{}…", preview)
                            } else {
                                preview
                            }
                        }
                    };
                    print!(" {}", dim(&format!("{}: {}", k, val_str)));
                }
            }
            println!();
            std::io::stdout().flush()?;

            let output = registry.execute(&tc.name, tc.input.clone(), ctx).await?;
            let tool_elapsed = tool_start.elapsed();

            let preview: String = output.text.chars().take(200).collect();
            let truncated = if output.text.len() > 200 { "…" } else { "" };
            if output.is_error {
                println!(
                    "  {} {} {}",
                    red("✗"),
                    preview,
                    dim(&format!("({})", format_elapsed(tool_elapsed)))
                );
            } else {
                println!(
                    "  {} {}{} {}",
                    green("✓"),
                    preview,
                    truncated,
                    dim(&format!("({})", format_elapsed(tool_elapsed)))
                );
            }
            session.add_tool_result(&tc.id, &output);
        }

        // Separator between tool execution and next LLM call
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
