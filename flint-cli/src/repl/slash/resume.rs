//! `/resume` — restore a saved session.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct ResumeCommand;


#[async_trait]
impl SlashCommand for ResumeCommand {
    fn name(&self) -> &str { "resume" }

    fn help(&self) -> &str {
        "Restore a saved session"
    }

    fn needs_llm(&self) -> bool { true }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        // 直接搬移 dispatch_resume 的 body
        match ctx.arg.clone() {
            Some(id) => {
                let session_dir = &ctx.config.session.path;
                let flint_sessions = flint_agent::Session::list_sessions(session_dir).unwrap_or_default();
                let found_flint = flint_sessions.iter().find(|s| s.id.starts_with(&id));
                let claude_sessions = crate::session_import::list_claude_sessions(ctx.working_dir).unwrap_or_default();
                let found_claude = claude_sessions.iter().find(|(_, m)| m.id.starts_with(&id));

                if let Some(meta) = found_flint {
                    let path = session_dir.join(format!("{}.json", meta.id));
                    match flint_agent::Session::load(&path) {
                        Ok((loaded_session, loaded_meta)) => {
                            *ctx.session = loaded_session;
                            *ctx.current_session_meta = Some(loaded_meta.clone());
                            println!("Resumed session: {} ({})", loaded_meta.title, &loaded_meta.id[..8]);
                            println!("  Provider: {} / {}", loaded_meta.provider, loaded_meta.model);
                            println!("  Messages: {}\n", loaded_meta.message_count);
                        }
                        Err(e) => println!("Error loading session: {}\n", e),
                    }
                } else if let Some((path, _meta)) = found_claude {
                    match crate::session_import::import_session(path) {
                        Ok((loaded_session, loaded_meta)) => {
                            *ctx.session = loaded_session;
                            *ctx.current_session_meta = Some(loaded_meta.clone());
                            let id_display = &loaded_meta.id[..8.min(loaded_meta.id.len())];
                            println!("Resumed Claude Code session: {} ({})", loaded_meta.title, id_display);
                            println!("  Provider: {} / {}", loaded_meta.provider, loaded_meta.model);
                            println!("  Messages: {}\n", loaded_meta.message_count);
                        }
                        Err(e) => println!("Error loading Claude Code session: {}\n", e),
                    }
                } else {
                    println!("Session not found: {}\n", id);
                }
            }
            None => match crate::resume_ui::run(ctx.config, ctx.working_dir) {
                Ok(Some((path, meta))) => {
                    let (loaded_session, loaded_meta) = if meta.provider == "claude-code" {
                        crate::session_import::import_session(&path)?
                    } else {
                        flint_agent::Session::load(&path)?
                    };
                    *ctx.session = loaded_session;
                    *ctx.current_session_meta = Some(loaded_meta.clone());
                    let id_display = &loaded_meta.id[..8.min(loaded_meta.id.len())];
                    let prefix = if meta.provider == "claude-code" {
                        "Resumed Claude Code session"
                    } else {
                        "Resumed session"
                    };
                    println!("{}: {} ({})", prefix, loaded_meta.title, id_display);
                    println!("  Provider: {} / {}", loaded_meta.provider, loaded_meta.model);
                    println!("  Messages: {}\n", loaded_meta.message_count);
                    crate::display::print_conversation_history(ctx.session);
                }
                Ok(None) => println!("Cancelled.\n"),
                Err(e) => println!("Error: {}\n", e),
            },
        }
        Ok(CommandResult::Continue)
    }
}

pub static RESUME_COMMAND: ResumeCommand = ResumeCommand;
