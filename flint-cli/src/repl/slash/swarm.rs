//! `/swarm` — show swarm status and manage agents.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct SwarmCommand;


#[async_trait]
impl SlashCommand for SwarmCommand {
    fn name(&self) -> &str { "swarm" }

    fn help(&self) -> &str {
        "Manage swarm agents"
    }

    fn needs_llm(&self) -> bool {
        false
    }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        dispatch_swarm(None, ctx).await;
        Ok(CommandResult::Continue)
    }
}

pub static SWARM_COMMAND: SwarmCommand = SwarmCommand;

async fn dispatch_swarm(sub: Option<String>, sc: &mut SlashContext<'_>) {
    let swarm = match sc.swarm {
        Some(ref s) => s,
        None => {
            println!("Swarm is disabled. Enable it in config: [features.swarm] enabled = true\n");
            return;
        }
    };

    match sub.as_deref() {
        Some(s) if s.starts_with("spawn") => {
            // /swarm spawn <prompt> — directly spawn a terminal sub-agent for testing
            let prompt = s.strip_prefix("spawn").unwrap_or("").trim().to_string();
            let prompt = if prompt.is_empty() {
                "Hello from the coordinator! Please introduce yourself and confirm you are running in a new terminal."
                    .to_string()
            } else {
                prompt
            };
            let mut sm = swarm.lock().unwrap();
            match sm.spawn_terminal(prompt, None, false, None) {
                Ok(result) => {
                    println!(
                        "Spawned terminal agent [{}] (task {})\n\
                         A new terminal window should appear.\n\
                         Use /swarm status to check progress.\n",
                        &result.agent_id[result.agent_id.len() - 4..],
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
        Some("files") | Some("file") => {
            let sm = swarm.lock().unwrap();
            let summary = sm.get_file_access_summary();
            if summary.is_empty() {
                println!("No files currently being accessed by agents.\n");
            } else {
                println!("Agent file activity:");
                for (agent_id, files) in &summary {
                    let short_id = agent_id.strip_prefix("agent_").unwrap_or(agent_id);
                    let short_id = &short_id[..4.min(short_id.len())];
                    println!("  {}: {}", short_id, files.join(", "));
                }
                println!();
            }
        }
        Some("help") => {
            println!(
                "\
Swarm commands:
  /swarm              Show swarm status
  /swarm spawn [task] Spawn terminal sub-agent (for testing)
  /swarm status       Show agents and tasks
  /swarm tasks        List all tasks
  /swarm files        Show which files agents are accessing
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
            let file_summary = sm.get_file_access_summary();
            let files_count: usize = file_summary.values().map(|v| v.len()).sum();
            println!(
                "Swarm Status:\n  Active agents: {}\n  Total tasks: {}\n  Files in use: {}\n\n\
                 Use /swarm status for details.\n\
                 Use /swarm files to see file activity.\n\
                 Use /swarm help for all commands.\n",
                sm.active_agent_count(),
                tasks.len(),
                files_count
            );
        }
    }
}
