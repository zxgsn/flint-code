//! `/mcp` — show MCP server status.

use async_trait::async_trait;
use super::{SlashCommand, SlashContext, CommandResult};
use anyhow::Result;

pub struct McpCommand;


#[async_trait]
impl SlashCommand for McpCommand {
    fn name(&self) -> &str { "mcp" }

    fn help(&self) -> &str {
        "Show MCP server status"
    }

    async fn execute(&self, ctx: &mut SlashContext<'_>) -> Result<CommandResult> {
        let status = ctx.mcp_manager.status();
        if status.is_empty() {
            println!("No MCP servers configured.");
            println!("Add [mcp_servers.<id>] to .flint.toml:\n");
            println!("  [mcp_servers.memory]");
            println!("  command = \"npx\"");
            println!("  args = [\"-y\", \"@modelcontextprotocol/server-memory\"]\n");
        } else {
            println!("MCP Servers:");
            for (id, count) in &status {
                println!("  + {} ({} tools)", id, count);
            }
            println!();
        }
        Ok(CommandResult::Continue)
    }
}

pub static MCP_COMMAND: McpCommand = McpCommand;
