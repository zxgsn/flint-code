//! `/status` — show current status.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct StatusCommand;


#[async_trait]
impl SlashCommand for StatusCommand {
    fn name(&self) -> &str { "status" }

    fn help(&self) -> &str {
        "Show current status"
    }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        crate::display::print_status(
            ctx.config,
            ctx.working_dir,
            ctx.turn_count,
            ctx.total_tool_calls,
            ctx.session.messages.len(),
        );
        println!();
        Ok(CommandResult::Continue)
    }
}

pub static STATUS_COMMAND: StatusCommand = StatusCommand;
