//! `/config` — reload configuration.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;
use flint_provider::Provider;
use std::sync::Arc;
use std::sync::Mutex;

pub struct ConfigCommand;


#[async_trait]
impl SlashCommand for ConfigCommand {
    fn name(&self) -> &str { "config" }

    fn help(&self) -> &str {
        "Reload configuration"
    }

    fn needs_llm(&self) -> bool { true }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        let old_type = ctx.config.provider.r#type.clone();
        let old_model = ctx.config.provider.model.clone();
        crate::cmd_config(ctx.working_dir)?;
        *ctx.config = flint_config::load(Some(ctx.working_dir))?;

        // Rebuild provider if type or model changed
        if ctx.config.provider.r#type != old_type || ctx.config.provider.model != old_model {
            let env_path = crate::provider::resolve_env_path(ctx.working_dir);
            crate::provider::load_env_override(&env_path);
            match crate::provider::build_provider_with_config(
                &ctx.config.provider.r#type,
                &ctx.config.provider.model,
                &ctx.config.provider.model_base_urls,
                &ctx.config.provider.model_api_keys,
            ) {
                Ok(p) => {
                    *ctx.prov = Arc::from(p);
                    eprintln!("Provider: {} / {}\n", ctx.config.provider.r#type, ctx.config.provider.model);
                }
                Err(e) => eprintln!("Failed to rebuild provider: {}\n", e),
            }
        }

        // Re-initialize memory if it was just enabled
        if ctx.config.features.is_enabled(flint_config::Feature::Memory) && ctx.memory.is_none() {
            let mem_config = flint_memory::MemoryConfig {
                max_core_blocks: ctx.config.features.memory.max_core_blocks,
                max_block_chars: ctx.config.features.memory.max_block_chars,
                auto_extract: ctx.config.features.memory.auto_extract,
                search_limit: ctx.config.features.memory.search_limit,
                ..Default::default()
            };
            match flint_memory::MemoryManager::new(mem_config, Some(ctx.working_dir)) {
                Ok(mm) => {
                    let shared = Arc::new(Mutex::new(mm));
                    crate::tools::register_memory_tools(ctx.registry, shared.clone());
                    *ctx.memory = Some(shared);
                    eprintln!("Memory: enabled (core + archival)");
                }
                Err(e) => eprintln!("Memory: failed to initialize: {}", e),
            }
        }

        // Re-initialize swarm if it was just enabled, or update existing swarm's model
        if ctx.config.features.is_enabled(flint_config::Feature::Swarm) {
            if let Some(ref swarm_arc) = ctx.swarm {
                let new_provider: Arc<dyn Provider> =
                    if let Some(ref swarm_model) = ctx.config.features.swarm.model {
                        match crate::provider::build_provider_with_config(
                            &ctx.config.provider.r#type,
                            swarm_model,
                            &ctx.config.provider.model_base_urls,
                            &ctx.config.provider.model_api_keys,
                        ) {
                            Ok(p) => Arc::from(p),
                            Err(e) => {
                                eprintln!("Warning: failed to build swarm model ({}), using main model", e);
                                ctx.prov.clone()
                            }
                        }
                    } else {
                        ctx.prov.clone()
                    };
                if let Ok(mut swarm) = swarm_arc.lock() {
                    swarm.set_provider(new_provider);
                    eprintln!("Swarm: updated default model");
                }
            } else {
                let swarm_config = flint_swarm::SwarmConfig {
                    max_agents: ctx.config.features.swarm.max_agents,
                    agent_max_turns: ctx.config.features.swarm.agent_max_turns,
                    max_output_chars: ctx.config.agent.max_output_chars,
                    open_viewer: true,
                };
                let (output_tx, output_rx) = flint_swarm::output::channel();
                tokio::spawn(flint_swarm::output::display_loop(output_rx));
                let sub_agent_prov: Arc<dyn Provider> =
                    if let Some(ref swarm_model) = ctx.config.features.swarm.model {
                        match crate::provider::build_provider_with_config(
                            &ctx.config.provider.r#type,
                            swarm_model,
                            &ctx.config.provider.model_base_urls,
                            &ctx.config.provider.model_api_keys,
                        ) {
                            Ok(p) => Arc::from(p),
                            Err(e) => {
                                eprintln!("Warning: failed to build swarm model ({}), using main model", e);
                                ctx.prov.clone()
                            }
                        }
                    } else {
                        ctx.prov.clone()
                    };

                let sub_agent_registry = ctx.registry.clone();
                let manager = flint_swarm::SwarmManager::new(
                    swarm_config,
                    sub_agent_prov,
                    ctx.working_dir.to_path_buf(),
                    ctx.system.to_string(),
                    output_tx,
                    sub_agent_registry,
                    None,
                );
                let shared = Arc::new(Mutex::new(manager));

                let agent_models: Vec<String> = ctx.config.features.swarm.agents.iter()
                    .map(|p| p.model.clone())
                    .collect();
                let swarm_prov_type = ctx.config.provider.r#type.clone();
                let swarm_model_base_urls = ctx.config.provider.model_base_urls.clone();
                let swarm_model_api_keys = ctx.config.provider.model_api_keys.clone();
                let build_provider: flint_swarm::ProviderFactory = Box::new(move |model: &str| {
                    crate::provider::build_provider_with_config(
                        &swarm_prov_type,
                        model,
                        &swarm_model_base_urls,
                        &swarm_model_api_keys,
                    )
                    .ok()
                    .map(|p| Arc::from(p) as Arc<dyn Provider>)
                });

                flint_swarm::register_swarm_tools(
                    ctx.registry,
                    shared.clone(),
                    None,
                    ctx.config.features.swarm.spawn_mode.clone(),
                    ctx.config.features.swarm.model.clone(),
                    agent_models,
                    build_provider,
                    ctx.config.features.swarm.model_selection.clone(),
                );
                *ctx.swarm = Some(shared);
                eprintln!("Swarm: enabled (max {} agents)", ctx.config.features.swarm.max_agents);
            }
        }

        println!();
        Ok(CommandResult::Continue)
    }
}

pub static CONFIG_COMMAND: ConfigCommand = ConfigCommand;
