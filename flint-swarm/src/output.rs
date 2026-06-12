//! Real-time sub-agent output display.
//!
//! Sub-agents send output events through an mpsc channel.
//! A display task prints them with [agent_id] prefixes.

use std::io::Write;
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Global lock for line-level terminal output.
static PRINT_LOCK: Mutex<()> = Mutex::new(());

/// Output event from a sub-agent.
#[derive(Debug)]
pub enum OutputEvent {
    Started { agent_id: String, task_id: String },
    Thinking { agent_id: String },
    TextDelta { agent_id: String, text: String },
    ToolResult { agent_id: String, success: bool, preview: String },
    Done { agent_id: String, elapsed: String, chars: usize },
    Failed { agent_id: String, error: String },
}

pub type OutputSender = mpsc::Sender<OutputEvent>;

pub fn channel() -> (OutputSender, mpsc::Receiver<OutputEvent>) {
    mpsc::channel(256)
}

fn dim(s: &str) -> String { format!("\x1b[90m{}\x1b[0m", s) }
fn green(s: &str) -> String { format!("\x1b[32m{}\x1b[0m", s) }
fn red(s: &str) -> String { format!("\x1b[31m{}\x1b[0m", s) }
fn cyan(s: &str) -> String { format!("\x1b[36m{}\x1b[0m", s) }
fn bold(s: &str) -> String { format!("\x1b[1m{}\x1b[0m", s) }

fn agent_tag(agent_id: &str) -> String {
    let short = agent_id.strip_prefix("agent_").unwrap_or(agent_id);
    format!("[{}]", &short[..4.min(short.len())])
}

fn print_line(agent_id: &str, line: &str) {
    let _lock = PRINT_LOCK.lock().unwrap();
    eprintln!("  {} {}", cyan(&agent_tag(agent_id)), line);
}

/// Display task: reads events and prints them. Run as a tokio task.
pub async fn display_loop(mut rx: mpsc::Receiver<OutputEvent>) {
    while let Some(event) = rx.recv().await {
        match event {
            OutputEvent::Started { agent_id, task_id } => {
                print_line(&agent_id, &format!("{} task {}", bold("started"), &task_id[..8.min(task_id.len())]));
            }
            OutputEvent::Thinking { agent_id } => {
                print_line(&agent_id, &dim("thinking..."));
            }
            OutputEvent::TextDelta { agent_id, text } => {
                let _lock = PRINT_LOCK.lock().unwrap();
                let tag = agent_tag(&agent_id);
                for (i, line) in text.split('\n').enumerate() {
                    if i > 0 { eprintln!(); }
                    if !line.is_empty() {
                        eprint!("  {} {}", cyan(&tag), line);
                    }
                }
                let _ = std::io::stderr().flush();
            }
            OutputEvent::ToolResult { agent_id, success, preview } => {
                let icon = if success { green("+") } else { red("x") };
                print_line(&agent_id, &format!("{} {}", icon, preview));
            }
            OutputEvent::Done { agent_id, elapsed, chars } => {
                print_line(&agent_id, &green(&format!("done ({}) · {} chars", elapsed, chars)));
            }
            OutputEvent::Failed { agent_id, error } => {
                print_line(&agent_id, &red(&format!("failed: {}", error)));
            }
        }
    }
}
