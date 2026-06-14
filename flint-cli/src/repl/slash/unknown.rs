//! `/unknown` — handle unrecognized commands.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct UnknownCommand {
    pub cmd: String,
}


#[async_trait]
impl SlashCommand for UnknownCommand {
    fn name(&self) -> &str { "__unknown__" }

    fn help(&self) -> &str {
        "" // hidden from /help
    }

    async fn execute(&self, _ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        println!(
            "Unknown command: /{}\nType /help for available commands.\n",
            self.cmd
        );
        Ok(CommandResult::Continue)
    }
}
