# flint

Lightweight LLM agent harness in Rust. Connects to an LLM, streams responses, and executes tool calls in a loop until the task is done.

Designed for **extensibility** — modular crates, feature toggles, clean separation of concerns.

## Quick Start

```bash
# Install
.\scripts\install.ps1          # Windows
bash scripts/install.sh        # Linux/macOS

# Run (launches setup wizard if no API key found)
flint

# Or with cargo
cargo run -- "explain this code"
```

## Architecture

```
flint (Cargo workspace)
├── flint-types      — Core types (Message, ToolDefinition, StreamEvent)
├── flint-config     — TOML config, feature toggles, skill loading
├── flint-provider   — LLM provider abstraction (OpenAI, Anthropic)
├── flint-agent      — Agent loop (run_turn), Session, Tool trait
├── flint-memory     — Three-layer memory system (Core/Archival/Recall)
├── flint-mcp        — MCP client (JSON-RPC over stdio)
├── flint-swarm      — Swarm coordination (multi-agent parallel execution)
└── flint-cli        — Binary, REPL, TUI, tools
```

## Features

### Core

- **Interactive REPL** — line editing, history, tab completion, syntax highlighting
- **Multi-provider** — OpenAI (and compatible), Anthropic, easy switching
- **Skills system** — reusable prompt modules from `.md` files, auto-injected on match
- **Modular config** — TOML config + TUI panel, per-feature toggles
- **Session persistence** — save/resume conversations, import from Claude Code

### Memory System (three-layer, inspired by Letta/MemGPT)

```toml
[features.memory]
enabled = true
max_core_blocks = 8      # Always-in-context blocks
auto_extract = true       # Auto-extract facts after each turn
```

- **Core memory** — always visible in system prompt (persona, user, project)
- **Archival memory** — long-term searchable store with TF-IDF scoring
- **Recall memory** — session-local extracted facts

Tools: `memory_remember`, `memory_search`, `memory_list`, `memory_forget`, `memory_update_core`

### Swarm — Multi-Agent Parallel Execution

```toml
[features.swarm]
enabled = true
max_agents = 5
agent_max_turns = 20
```

Spawn sub-agents for parallel work. Each runs as an independent tokio task with its own Session, sharing the Provider via Arc. Real-time output displayed in separate terminal windows.

```
主终端                              新终端（每个 agent 一个）
┌────────────────────────────┐     ┌─ Agent [a1b2] ──────────────┐
│ ❯ 分析 A 和 B 模块          │     │ ~ thinking...               │
│                            │     │ * read src/a.rs              │
│   [a1b2] started           │     │   + 1523 bytes (0.3s)        │
│   [f4e5] started           │     │ -- turn complete · 1.5s --   │
│   [a1b2] done (2.1s)      │     │ === Result ===               │
│   [f4e5] done (1.8s)      │     │ A 模块包含 3 个函数...        │
│                            │     └─────────────────────────────┘
│   汇总结果...               │     ┌─ Agent [f4e5] ──────────────┐
└────────────────────────────┘     │ ...                         │
                                   └─────────────────────────────┘
```

**Swarm tools:**

| Command | Description |
|---------|-------------|
| `swarm spawn` | Spawn sub-agent, wait for result (blocking) |
| `swarm spawn async=true` | Non-blocking spawn, check later with `result` |
| `swarm followup` | Send follow-up to existing agent (preserves context) |
| `swarm result` | Get async task result |
| `swarm status` | Show agents and tasks |
| `swarm stop` | Stop agent(s) |
| `swarm viewer` | Open aggregated log viewer |

**Design highlights:**

- **In-process tokio tasks** — no separate processes, shared Provider via Arc
- **Session persistence** — sub-agents maintain conversation context across turns
- **Separate terminal output** — each agent's log tails in its own PowerShell/xterm window
- **Non-blocking callback** — uses `try_send` (safe in tokio runtime, no panics)
- **Dual mode** — blocking (parallel wait) and async (fire-and-check)

### MCP (Model Context Protocol)

```toml
[mcp_servers.memory]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-memory"]
```

Connect external tool servers via JSON-RPC over stdio. Tools auto-registered with `mcp__<server>__<tool>` naming.

### Built-in Tools

| Tool | Description |
|------|-------------|
| `read` | Read file contents |
| `write` | Write content to file |
| `edit` | Search-and-replace with uniqueness check |
| `bash` | Shell command execution (UTF-8 on Windows) |
| `grep` | Regex search via ripgrep |
| `glob` | File pattern matching |
| `web_fetch` | HTTP fetch with HTML-to-text |

## Usage

### REPL Commands

| Command | Description |
|---------|-------------|
| `/config` | Open settings panel (TUI) |
| `/setup` | Configure provider (edit existing config) |
| `/model` | Interactive model picker |
| `/model <name>` | Switch to specific model |
| `/skills` | List available skills |
| `/memory` | Memory status and management |
| `/swarm` | Swarm status and management |
| `/mcp` | MCP server status |
| `/compact` | Compress conversation history |
| `/resume` | Restore saved session |
| `/clear` | Clear conversation history |
| `/status` | Show current config |
| `/help` | Show help |
| `/quit` | Exit (or double Ctrl+C) |

### Provider Setup

```bash
# Interactive setup
flint setup

# Automated setup
flint setup --provider openai --key sk-xxx
flint setup --provider anthropic --key sk-ant-xxx --model claude-sonnet-4-20250514

# OpenAI-compatible endpoints
flint setup --provider openai --key sk-xxx --base-url https://proxy.example.com/v1
```

## Configuration

Config locations (project overrides user):

- **User-level**: `~/.flint/config.toml`
- **Project-level**: `.flint.toml`

```toml
[provider]
type = "openai"
model = "gpt-4o"

[agent]
max_turns = 50
max_output_chars = 65536
context_window_chars = 500000

[features.skills]
enabled = true

[features.memory]
enabled = true
auto_extract = true

[features.compaction]
enabled = true

[features.swarm]
enabled = true
max_agents = 5
agent_max_turns = 20

[mcp_servers.memory]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-memory"]
```

Features can be toggled at runtime via `/config` (TUI panel). Changes to `memory` and `swarm` take effect immediately without restart.

## Skills

Skills are reusable prompt modules. Drop `.md` files into:

- `~/.flint/skills/` (user-level)
- `.flint/skills/` (project-level)

```markdown
---
name: code-review
description: Review code for bugs
---
You are a code reviewer. Check for bugs and edge cases.
```

Skills are auto-injected when user input matches. Use `/skills` to see loaded skills.

## Project Structure

| Crate | Responsibility |
|-------|---------------|
| `flint-types` | Core shared types: Message, Role, ContentBlock, ToolDefinition, StreamEvent |
| `flint-config` | Two-tier TOML config merge, feature toggles, skill loading |
| `flint-provider` | LLM provider abstraction with OpenAI and Anthropic implementations, SSE streaming, retry logic |
| `flint-agent` | Core agent loop (`run_turn`), Session persistence, Tool trait and ToolRegistry |
| `flint-memory` | Three-layer memory system: core blocks, archival store, TF-IDF search |
| `flint-mcp` | MCP client for external tool servers (JSON-RPC over stdio) |
| `flint-swarm` | Multi-agent coordination: task registry, tokio task spawning, real-time output, viewer terminals |
| `flint-cli` | Binary entry point, REPL, built-in tools, TUI panels, system prompt building |

## Extending

### Add a Tool

```rust
use flint_agent::{Tool, ToolContext};
use flint_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;

struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "my_tool".into(),
            description: "Does something cool".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, _input: serde_json::Value, _ctx: &ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::text("done"))
    }
}
```

### Swap Provider

```rust
use flint_provider::Provider;
// Implement Provider trait for custom LLM backend
```

## License

MIT
