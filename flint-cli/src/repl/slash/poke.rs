//! `/poke` — manage auto-poke.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct PokeCommand;


#[async_trait]
impl SlashCommand for PokeCommand {
    fn name(&self) -> &str { "poke" }

    fn help(&self) -> &str {
        "Manage auto-poke (on, off, status, help)"
    }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        let ap = match &mut ctx.auto_poke {
            Some(ref mut ap) => ap,
            None => {
                println!("Auto-poke is not available (todo tool not registered).\n");
                return Ok(CommandResult::Continue);
            }
        };

        let sub = ctx.arg.as_deref();
        match sub {
            Some("on") | Some("enable") => {
                ap.enabled = true;
                ap.consecutive_pokes = 0;
                println!("Auto-poke: enabled (max {} consecutive pokes)\n", ap.max_pokes);
            }
            Some("off") | Some("disable") => {
                ap.enabled = false;
                println!("Auto-poke: disabled\n");
            }
            Some("status") => {
                let incomplete = flint_agent::todo::incomplete_count(&ap.store);
                println!(
                    "Auto-poke: {} | Pokes this round: {}/{} | Incomplete todos: {}\n",
                    if ap.enabled { "enabled" } else { "disabled" },
                    ap.consecutive_pokes,
                    ap.max_pokes,
                    incomplete,
                );
            }
            Some("help") => {
                println!(
                    "\
Auto-poke automatically sends a \"continue working\" message when
incomplete todos remain after a turn completes.

Commands:
  /poke on       Enable auto-poke
  /poke off      Disable auto-poke
  /poke status   Show current state
  /poke help     Show this help

Safety: stops after {} consecutive pokes without user input,
and stops immediately on non-retryable errors (auth, billing, etc.).\n",
                    ap.max_pokes
                );
            }
            _ => {
                ap.enabled = !ap.enabled;
                if ap.enabled {
                    ap.consecutive_pokes = 0;
                    println!("Auto-poke: enabled\n");
                } else {
                    println!("Auto-poke: disabled\n");
                }
            }
        }
        Ok(CommandResult::Continue)
    }
}

pub static POKE_COMMAND: PokeCommand = PokeCommand;
