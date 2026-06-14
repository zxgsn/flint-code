//! `/setup` — configure provider interactively.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;
use std::sync::Arc;

pub struct SetupCommand;

#[async_trait]
impl SlashCommand for SetupCommand {
    fn name(&self) -> &str { "setup" }

    fn help(&self) -> &str {
        "Configure provider interactively"
    }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        let env_path = ctx.working_dir.join(".env");
        crate::setup_ui::run_edit(&env_path)?;
        crate::provider::load_env_override(&env_path);
        let p_type = std::env::var("FLINT_PROVIDER")
            .unwrap_or_else(|_| ctx.config.provider.r#type.clone());
        let p_model = std::env::var("FLINT_MODEL")
            .unwrap_or_else(|_| ctx.config.provider.model.clone());
        match crate::provider::build_provider_with_config(&p_type, &p_model, &ctx.config.provider.model_base_urls, &ctx.config.provider.model_api_keys) {
            Ok(p) => {
                *ctx.prov = Arc::from(p);
                ctx.config.provider.r#type = p_type;
                ctx.config.provider.model = p_model;
                println!("Provider reloaded.\n");
            }
            Err(e) => println!("Setup incomplete: {}\n", e),
        }
        Ok(CommandResult::Continue)
    }
}

pub static SETUP_COMMAND: SetupCommand = SetupCommand;
