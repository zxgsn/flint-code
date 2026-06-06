//! MCP (Model Context Protocol) client for flint.
//!
//! Provides 4-layer integration:
//! 1. **Connection** — `McpClient` spawns server processes, JSON-RPC over stdio
//! 2. **Discovery** — `tools/list` → `ToolInfo` → `McpTool`
//! 3. **Adapter** — `McpTool` implements `Tool` trait, delegates to `tools/call`
//! 4. **Dispatch** — registered in `ToolRegistry`, dispatched by `run_turn()` as usual

pub mod client;
pub mod manager;
pub mod protocol;
pub mod tool;

pub use client::McpClient;
pub use manager::{McpManager, McpServerConfig};
pub use tool::McpTool;
