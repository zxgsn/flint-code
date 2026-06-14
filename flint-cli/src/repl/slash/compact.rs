//! `/compact` — summarize and compress conversation history.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;
use flint_agent::{run_turn, Session};

pub struct CompactCommand;


#[async_trait]
impl SlashCommand for CompactCommand {
    fn name(&self) -> &str { "compact" }

    fn help(&self) -> &str {
        "Summarize and compress conversation history"
    }

    fn needs_llm(&self) -> bool { true }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        if ctx.session.is_empty() {
            println!("Nothing to compact.\n");
            return Ok(CommandResult::Continue);
        }
        let msg_count = ctx.session.messages.len();
        eprintln!("Compacting {} messages...", msg_count);

        let mut history = String::new();
        for msg in &ctx.session.messages {
            let role = match msg.role {
                flint_types::Role::User => "User",
                flint_types::Role::Assistant => "Assistant",
                flint_types::Role::System => "System",
                flint_types::Role::Tool => "Tool",
            };
            let text = msg.text();
            if !text.is_empty() {
                history.push_str(&format!("{}: {}\n\n", role, text));
            }
        }

        let compact_prompt = format!(
            "Summarize the following conversation concisely. Keep all key facts, decisions, file paths, and code context. Output only the summary, no preamble.\n\n{}",
            history
        );

        let mut compact_session = Session::new();
        compact_session.add_user(&compact_prompt);
        match run_turn(
            ctx.prov.as_ref(),
            &mut compact_session,
            ctx.registry,
            "You are a summarizer. Be concise.",
            ctx.ctx,
            5,
            None,
            65536,
            true, // silent
            None,
            None,
        )
        .await
        {
            Ok((summary, _)) => {
                if summary.is_empty() {
                    println!("Compaction failed: empty summary.\n");
                    return Ok(CommandResult::Continue);
                }
                let keep = 4usize.min(msg_count);
                let tail: Vec<flint_types::Message> =
                    ctx.session.messages[msg_count - keep..].to_vec();
                *ctx.session = Session::new();
                ctx.session
                    .messages
                    .push(flint_types::Message::system(&format!(
                        "[Compacted context from {} messages]\n\n{}",
                        msg_count, summary
                    )));
                ctx.session.messages.extend(tail);
                println!(
                    "Compacted {} -> {} messages.\n",
                    msg_count,
                    ctx.session.messages.len()
                );
            }
            Err(e) => {
                println!("Compaction failed: {}\n", e);
            }
        }
        Ok(CommandResult::Continue)
    }
}

pub static COMPACT_COMMAND: CompactCommand = CompactCommand;
