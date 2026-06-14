//! `/help` — show available commands.
//!
//! Aliases: h, ?

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct HelpCommand;


#[async_trait]
impl SlashCommand for HelpCommand {
    fn name(&self) -> &str { "help" }

    fn aliases(&self) -> &[&str] {
        &["h", "?"]
    }

    fn help(&self) -> &str {
        "Show available commands"
    }

    async fn execute(&self, _ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        crate::display::print_help();
        Ok(CommandResult::Continue)
    }
}

pub static HELP_COMMAND: HelpCommand = HelpCommand;
