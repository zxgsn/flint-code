//! `/undo` — revert the last turn.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct UndoCommand;


#[async_trait]
impl SlashCommand for UndoCommand {
    fn name(&self) -> &str { "undo" }

    fn help(&self) -> &str {
        "Revert the last turn"
    }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        crate::repl::perform_undo(&ctx.checkpoint_store, ctx.session, ctx.working_dir);
        Ok(CommandResult::Continue)
    }
}

pub static UNDO_COMMAND: UndoCommand = UndoCommand;
