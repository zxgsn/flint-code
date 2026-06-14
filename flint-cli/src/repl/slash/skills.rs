//! `/skills` — list available skills.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct SkillsCommand;


#[async_trait]
impl SlashCommand for SkillsCommand {
    fn name(&self) -> &str { "skills" }

    fn help(&self) -> &str {
        "List available skills"
    }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        crate::display::print_skills(ctx.config, ctx.working_dir);
        Ok(CommandResult::Continue)
    }
}

pub static SKILLS_COMMAND: SkillsCommand = SkillsCommand;
