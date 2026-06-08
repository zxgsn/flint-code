//! Built-in tools for flint.
//!
//! These tools are always available to the agent: read, write, bash, grep, glob.

use anyhow::Result;
use async_trait::async_trait;
use flint_agent::{Tool, ToolContext, ToolRegistry};
use flint_types::{ToolDefinition, ToolOutput};

// ── Read ───────────────────────────────────────────────────────────────────

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".into(),
            description: "Read a file's contents. Input: {\"path\": \"...\"}".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to read" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'path'"))?;
        let full = ctx.working_dir.join(path);
        match std::fs::read_to_string(&full) {
            Ok(content) => Ok(ToolOutput::text(content)),
            Err(e) => Ok(ToolOutput::error(format!("{}: {}", full.display(), e))),
        }
    }
}

// ── Write ──────────────────────────────────────────────────────────────────

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".into(),
            description: "Write content to a file. Input: {\"path\": \"...\", \"content\": \"...\"}"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to write" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'path'"))?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'content'"))?;
        let full = ctx.working_dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&full, content)?;
        Ok(ToolOutput::text(format!("wrote {}", full.display())))
    }
}

// ── Bash ───────────────────────────────────────────────────────────────────

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".into(),
            description: "Run a shell command. Input: {\"command\": \"...\"}".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to run" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'command'"))?;

        let output = if cfg!(target_os = "windows") {
            // chcp 65001 forces UTF-8 output from cmd.exe, avoiding GBK mojibake
            let wrapped = format!("chcp 65001 >nul && {}", command);
            std::process::Command::new("cmd")
                .args(["/C", &wrapped])
                .current_dir(&ctx.working_dir)
                .output()?
        } else {
            std::process::Command::new("sh")
                .args(["-c", command])
                .current_dir(&ctx.working_dir)
                .output()?
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}{}", stdout, stderr);

        if output.status.success() {
            Ok(ToolOutput::text(combined))
        } else {
            Ok(ToolOutput::error(format!(
                "exit code: {}\n{}",
                output.status.code().unwrap_or(-1),
                combined
            )))
        }
    }
}

// ── Grep ───────────────────────────────────────────────────────────────────

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".into(),
            description: "Search for a pattern in files. Input: {\"pattern\": \"...\", \"path\": \"...\"}".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search" },
                    "path": { "type": "string", "description": "Directory or file to search in" }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'pattern'"))?;
        let path = input["path"].as_str().unwrap_or(".");
        let full = ctx.working_dir.join(path);

        let output = std::process::Command::new("rg")
            .args(["--no-heading", "-n", pattern, full.to_str().unwrap_or(".")])
            .output();

        match output {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                if text.is_empty() {
                    Ok(ToolOutput::text("no matches found"))
                } else {
                    Ok(ToolOutput::text(text.to_string()))
                }
            }
            Err(_) => Ok(ToolOutput::error("ripgrep (rg) not found in PATH")),
        }
    }
}

// ── Glob ───────────────────────────────────────────────────────────────────

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "glob".into(),
            description: "Find files matching a glob pattern. Input: {\"pattern\": \"...\"}".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern, e.g. **/*.rs" }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'pattern'"))?;
        let full_pattern = ctx.working_dir.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let paths: Vec<String> = glob::glob(&pattern_str)
            .map_err(|e| anyhow::anyhow!("invalid glob: {}", e))?
            .filter_map(|p| p.ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        if paths.is_empty() {
            Ok(ToolOutput::text("no files matched"))
        } else {
            Ok(ToolOutput::text(paths.join("\n")))
        }
    }
}

// ── Registration helper ────────────────────────────────────────────────────

/// Register all built-in tools into the registry.
pub fn register_builtins(registry: &mut ToolRegistry) {
    registry.register(ReadTool);
    registry.register(WriteTool);
    registry.register(BashTool);
    registry.register(GrepTool);
    registry.register(GlobTool);
}
