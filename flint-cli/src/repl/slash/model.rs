//! `/model` — switch model.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;
use flint_provider::Provider;
use std::sync::Arc;

pub struct ModelCommand;


#[async_trait]
impl SlashCommand for ModelCommand {
    fn name(&self) -> &str { "model" }

    fn help(&self) -> &str {
        "Switch model"
    }

    fn needs_llm(&self) -> bool { true }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        match ctx.arg.clone() {
            Some(m) => {
                match crate::provider::build_provider_with_config(
                    &ctx.config.provider.r#type,
                    &m,
                    &ctx.config.provider.model_base_urls,
                    &ctx.config.provider.model_api_keys,
                ) {
                    Ok(p) => {
                        let new_provider: Arc<dyn Provider> = Arc::from(p);
                        *ctx.prov = new_provider.clone();
                        ctx.config.provider.model = m.clone();
                        std::env::set_var("FLINT_MODEL", &m);
                        if !crate::model_ui::is_preset(&ctx.config.provider.r#type, &m)
                            && !ctx.config.provider.recent_models.contains(&m)
                        {
                            ctx.config.provider.recent_models.push(m.clone());
                        }
                        if let Some(ref swarm_arc) = ctx.swarm {
                            if let Ok(mut swarm) = swarm_arc.lock() {
                                swarm.set_provider(new_provider.clone());
                                eprintln!("Swarm: updated default model to {}", m);
                            }
                        }
                        let _ = ctx.config.save(&ctx.working_dir.join(".flint.toml"));
                        println!("Switched to model: {}\n", m);
                    }
                    Err(e) => println!("Failed to switch model: {}\n", e),
                }
            }
            None => {
                let recent = ctx.config.provider.recent_models.clone();
                match crate::model_ui::run(
                    &ctx.config.provider.r#type,
                    &ctx.config.provider.model,
                    &recent,
                ) {
                    Ok(Some((m, is_custom, updated_recent))) => {
                        ctx.config.provider.recent_models = updated_recent;
                        if is_custom {
                            let env_path = ctx.working_dir.join(".env");
                            println!("Custom model: {} -- opening provider setup...\n", m);
                            match crate::setup_ui::run(&env_path) {
                                Ok(true) => {
                                    crate::provider::load_env_override(&env_path);
                                    let p_type = std::env::var("FLINT_PROVIDER")
                                        .unwrap_or_else(|_| ctx.config.provider.r#type.clone());
                                    match crate::provider::build_provider_with_config(
                                        &p_type,
                                        &m,
                                        &ctx.config.provider.model_base_urls,
                                        &ctx.config.provider.model_api_keys,
                                    ) {
                                        Ok(p) => {
                                            *ctx.prov = Arc::from(p);
                                            ctx.config.provider.r#type = p_type;
                                            ctx.config.provider.model = m.clone();
                                            std::env::set_var("FLINT_MODEL", &m);
                                            let _ = ctx.config.save(&ctx.working_dir.join(".flint.toml"));
                                            println!("Switched to model: {}\n", m);
                                        }
                                        Err(e) => println!("Failed to switch model: {}\n", e),
                                    }
                                }
                                Ok(false) => println!("Setup cancelled. Model not changed.\n"),
                                Err(e) => println!("Setup error: {}\n", e),
                            }
                        } else {
                            match crate::provider::build_provider_with_config(
                                &ctx.config.provider.r#type,
                                &m,
                                &ctx.config.provider.model_base_urls,
                                &ctx.config.provider.model_api_keys,
                            ) {
                                Ok(p) => {
                                    *ctx.prov = Arc::from(p);
                                    ctx.config.provider.model = m.clone();
                                    std::env::set_var("FLINT_MODEL", &m);
                                    let _ = ctx.config.save(&ctx.working_dir.join(".flint.toml"));
                                    println!("Switched to model: {}\n", m);
                                }
                                Err(e) => println!("Failed to switch model: {}\n", e),
                            }
                        }
                    }
                    Ok(None) => println!("Cancelled.\n"),
                    Err(e) => println!("Error: {}\n", e),
                }
            }
        }
        Ok(CommandResult::Continue)
    }
}

pub static MODEL_COMMAND: ModelCommand = ModelCommand;
