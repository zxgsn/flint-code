//! `/memory` — show memory status, list, search, or edit core blocks.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct MemoryCommand;


#[async_trait]
impl SlashCommand for MemoryCommand {
    fn name(&self) -> &str { "memory" }

    fn aliases(&self) -> &[&str] {
        &["mem"]
    }

    fn help(&self) -> &str {
        "Manage memory (list, core, help)"
    }

    fn needs_llm(&self) -> bool {
        false
    }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        dispatch_memory(None, ctx).await;
        Ok(CommandResult::Continue)
    }
}

pub static MEMORY_COMMAND: MemoryCommand = MemoryCommand;

async fn dispatch_memory(sub: Option<String>, sc: &mut SlashContext<'_>) {
    let mem = match sc.memory {
        Some(ref m) => m,
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
