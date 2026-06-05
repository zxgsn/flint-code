# flint

Lightweight LLM agent harness in Rust. Connects to an LLM, streams responses, and executes tool calls (file read/write, shell, grep, glob) in a loop until the task is done.

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

## Features

- **Interactive REPL** — slash commands (`/config`, `/setup`, `/model`, `/skills`)
- **Multi-provider** — OpenAI (and compatible), Anthropic, easy switching
- **Skills system** — reusable prompt modules from `.md` files
- **Modular config** — TOML config + TUI panel, feature toggles
- **First-run wizard** — guided setup when no API key detected

## Usage

### REPL Commands

| Command | Description |
|---------|-------------|
| `/config` | Open settings panel |
| `/setup` | Configure provider credentials |
| `/model` | Interactive model picker |
| `/model <name>` | Switch to specific model |
| `/skills` | List available skills |
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

Credentials are saved to `.env` in the working directory.

## Configuration

Config locations (project overrides user):

- **User-level**: `~/.flint/config.toml`
- **Project-level**: `.flint.toml`

```toml
[provider]
type = "openai"
model = "gpt-4o"

[features.skills]
enabled = true

[features.memory]
enabled = true
```

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
