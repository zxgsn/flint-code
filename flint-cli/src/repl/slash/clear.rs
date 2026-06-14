//! `/clear` — clear the current session.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;
use flint_agent::Session;

pub struct ClearCommand;


#[async_trait]
impl SlashCommand for ClearCommand {
    fn name(&self) -> &str { "clear" }

    fn help(&self) -> &str {
        "Clear current session"
    }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        *ctx.session = Session::new();
        *ctx.current_session_meta = None;
        println!("Session cleared.\n");
        Ok(CommandResult::Continue)
    }
}

pub static CLEAR_COMMAND: ClearCommand = ClearCommand;
