//! `/quit` — exit the REPL.
//!
//! Aliases: quit, exit, q

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct QuitCommand;


#[async_trait]
impl SlashCommand for QuitCommand {
    fn name(&self) -> &str { "quit" }

    fn aliases(&self) -> &[&str] {
        &["exit", "q"]
    }

    fn help(&self) -> &str {
        "Exit the REPL"
    }

    async fn execute(&self, _ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        Ok(CommandResult::Quit)
    }
}

pub static QUIT_COMMAND: QuitCommand = QuitCommand;
