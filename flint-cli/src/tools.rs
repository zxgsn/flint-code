//! Built-in tools for flint.
//!
//! Core tools are always available: read, write, bash, grep, glob.
//! Memory tools are registered when the memory feature is enabled.

use anyhow::Result;
use async_trait::async_trait;
use flint_agent::{Tool, ToolContext, ToolRegistry};
use flint_types::{ToolDefinition, ToolOutput};
use std::sync::{Arc, Mutex};

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

// ── Edit ───────────────────────────────────────────────────────────────────

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit".into(),
            description: "Replace text in a file. Input: {\"path\": \"...\", \"old_string\": \"...\", \"new_string\": \"...\", \"replace_all\": false}. \
                old_string must be unique in the file unless replace_all is true."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to edit" },
                    "old_string": { "type": "string", "description": "Exact text to find and replace" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)", "default": false }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'path'"))?;
        let old_string = input["old_string"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'old_string'"))?;
        let new_string = input["new_string"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'new_string'"))?;
        let replace_all = input["replace_all"].as_bool().unwrap_or(false);

        if old_string.is_empty() {
            return Ok(ToolOutput::error("old_string cannot be empty"));
        }

        let full = ctx.working_dir.join(path);
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::error(format!("{}: {}", full.display(), e))),
        };

        let count = content.matches(old_string).count();

        if count == 0 {
            return Ok(ToolOutput::error(format!(
                "old_string not found in {}",
                full.display()
            )));
        }

        if !replace_all && count > 1 {
            return Ok(ToolOutput::error(format!(
                "old_string found {} times in {}. Must be unique. Use replace_all=true or provide more context.",
                count,
                full.display()
            )));
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        std::fs::write(&full, &new_content)?;

        let action = if replace_all {
            format!("replaced {} occurrence(s)", count)
        } else {
            "replaced 1 occurrence".to_string()
        };
        Ok(ToolOutput::text(format!(
            "edited {}: {}",
            full.display(),
            action
        )))
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

// ── Web Fetch ──────────────────────────────────────────────────────────────

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_fetch".into(),
            description: "Fetch a URL and return its content as text. Input: {\"url\": \"...\", \"max_chars\": 50000}".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to fetch" },
                    "max_chars": { "type": "integer", "description": "Max characters to return (default 50000)", "default": 50000 }
                },
                "required": ["url"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let url = input["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'url'"))?;

        let max_chars = input["max_chars"].as_u64().unwrap_or(50000) as usize;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;

        let resp = client.get(url).send().await?;

        let status = resp.status();
        if !status.is_success() {
            return Ok(ToolOutput::error(format!(
                "HTTP {} for {}",
                status.as_u16(),
                url
            )));
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = resp.text().await?;

        // Basic HTML to text conversion
        let text = if content_type.contains("html") {
            html_to_text(&body)
        } else {
            body
        };

        if text.len() > max_chars {
            Ok(ToolOutput::text(format!(
                "{}\n\n[truncated — {} chars total]",
                &text[..max_chars],
                text.len()
            )))
        } else {
            Ok(ToolOutput::text(text))
        }
    }
}

/// Basic HTML to text conversion — strips tags, decodes common entities.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;

    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
            }
            '>' => {
                in_tag = false;
                continue;
            }
            _ if in_tag => continue,
            _ => {}
        }

        if !in_tag {
            out.push(c);
        }
    }

    // Decode common HTML entities
    out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'");

    // Collapse multiple newlines/spaces
    let mut result = String::new();
    let mut prev_newline = false;
    for line in out.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_newline {
                result.push('\n');
                prev_newline = true;
            }
        } else {
            result.push_str(trimmed);
            result.push('\n');
            prev_newline = false;
        }
    }

    result
}

// ── Memory Tools ───────────────────────────────────────────────────────────

type SharedMemory = Arc<Mutex<flint_memory::MemoryManager>>;

// ── memory_remember ────────────────────────────────────────────────────────

pub struct MemoryRememberTool {
    pub memory: SharedMemory,
}

#[async_trait]
impl Tool for MemoryRememberTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_remember".into(),
            description: "Save a fact, preference, correction, or pattern to long-term memory. \
                Input: {\"content\": \"...\", \"category\": \"fact|preference|correction|pattern\", \
                \"tags\": [\"...\"], \"scope\": \"project|global\"}"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "The fact or information to remember" },
                    "category": { "type": "string", "description": "Category: fact, preference, correction, pattern", "default": "fact" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags for searchability" },
                    "scope": { "type": "string", "description": "project (default) or global", "default": "project" }
                },
                "required": ["content"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'content'"))?;

        let category = flint_memory::MemoryCategory::from_str_loose(
            input["category"].as_str().unwrap_or("fact"),
        );

        let tags: Vec<String> = input["tags"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let scope = match input["scope"].as_str().unwrap_or("project") {
            "global" => flint_memory::MemoryScope::Global,
            _ => flint_memory::MemoryScope::Project,
        };

        let mut mm = self.memory.lock().unwrap();
        match mm.remember(content, category, tags, scope, flint_memory::TrustLevel::Medium) {
            Ok(id) => Ok(ToolOutput::text(format!("remembered: {} ({})", content, id))),
            Err(e) => Ok(ToolOutput::error(format!("failed to remember: {}", e))),
        }
    }
}

// ── memory_forget ──────────────────────────────────────────────────────────

pub struct MemoryForgetTool {
    pub memory: SharedMemory,
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_forget".into(),
            description: "Remove a memory by ID. Input: {\"id\": \"mem_...\"}".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Memory ID to forget" }
                },
                "required": ["id"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let id = input["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'id'"))?;

        let mut mm = self.memory.lock().unwrap();
        match mm.forget(id) {
            Ok(true) => Ok(ToolOutput::text(format!("forgotten: {}", id))),
            Ok(false) => Ok(ToolOutput::text(format!("memory not found: {}", id))),
            Err(e) => Ok(ToolOutput::error(format!("failed to forget: {}", e))),
        }
    }
}

// ── memory_search ──────────────────────────────────────────────────────────

pub struct MemorySearchTool {
    pub memory: SharedMemory,
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_search".into(),
            description: "Search long-term memories by keyword. \
                Input: {\"query\": \"...\", \"scope\": \"all|project|global\", \"limit\": 5}"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "scope": { "type": "string", "description": "all (default), project, or global", "default": "all" },
                    "limit": { "type": "integer", "description": "Max results (default 5)", "default": 5 }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let query = input["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'query'"))?;

        let scope = match input["scope"].as_str().unwrap_or("all") {
            "project" => Some(flint_memory::MemoryScope::Project),
            "global" => Some(flint_memory::MemoryScope::Global),
            _ => None,
        };

        let limit = input["limit"].as_u64().unwrap_or(5) as usize;

        let mut mm = self.memory.lock().unwrap();
        let results = mm.search(query, scope, Some(limit));

        if results.is_empty() {
            return Ok(ToolOutput::text("no memories found matching query"));
        }

        let mut output = String::new();
        for (i, result) in results.iter().enumerate() {
            let trust_label = match result.entry.trust {
                flint_memory::TrustLevel::High => "high",
                flint_memory::TrustLevel::Medium => "med",
                flint_memory::TrustLevel::Low => "low",
            };
            output.push_str(&format!(
                "{}. [{}][{}][{}] {} (id: {}, score: {:.2})\n",
                i + 1,
                result.entry.category,
                result.entry.scope,
                trust_label,
                result.entry.content,
                result.entry.id,
                result.score
            ));
        }

        Ok(ToolOutput::text(output))
    }
}

// ── memory_list ────────────────────────────────────────────────────────────

pub struct MemoryListTool {
    pub memory: SharedMemory,
}

#[async_trait]
impl Tool for MemoryListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_list".into(),
            description: "List all stored memories. \
                Input: {\"scope\": \"all|project|global\"}"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "description": "all (default), project, or global", "default": "all" }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let scope = match input["scope"].as_str().unwrap_or("all") {
            "project" => Some(flint_memory::MemoryScope::Project),
            "global" => Some(flint_memory::MemoryScope::Global),
            _ => None,
        };

        let mm = self.memory.lock().unwrap();
        let entries = mm.list(scope);

        if entries.is_empty() {
            return Ok(ToolOutput::text("no memories stored"));
        }

        let mut output = format!("{} memories:\n", entries.len());
        for entry in &entries {
            output.push_str(&format!(
                "- [{}][{}] {} (id: {}, accessed: {}x)\n",
                entry.category, entry.scope, entry.content, entry.id, entry.access_count
            ));
        }

        Ok(ToolOutput::text(output))
    }
}

// ── memory_update_core ─────────────────────────────────────────────────────

pub struct MemoryUpdateCoreTool {
    pub memory: SharedMemory,
}

#[async_trait]
impl Tool for MemoryUpdateCoreTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_update_core".into(),
            description: "Update a core memory block (always visible in system prompt). \
                Input: {\"label\": \"persona|user|project|...\", \"content\": \"...\"}"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "label": { "type": "string", "description": "Block label (e.g. persona, user, project)" },
                    "content": { "type": "string", "description": "New content for the block" }
                },
                "required": ["label", "content"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let label = input["label"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'label'"))?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'content'"))?;

        let mut mm = self.memory.lock().unwrap();
        match mm.update_core(label, content) {
            Ok(true) => Ok(ToolOutput::text(format!(
                "updated core block '{}'",
                label
            ))),
            Ok(false) => Ok(ToolOutput::error(format!(
                "failed to update block '{}' (read-only or too long)",
                label
            ))),
            Err(e) => Ok(ToolOutput::error(format!("failed to update core: {}", e))),
        }
    }
}

// ── Todo ─────────────────────────────────────────────────────────────────────

pub struct TodoTool {
    pub store: flint_agent::TodoStore,
}

#[async_trait]
impl Tool for TodoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "todo".into(),
            description: "Manage a session-scoped task list. Use this to track \
                multi-step work. Actions: add, update, list. \
                The auto-poke system will prompt you to continue when incomplete \
                todos remain after a turn."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "update", "list"],
                        "description": "add: create a new todo. update: change status. list: show all."
                    },
                    "title": { "type": "string", "description": "Todo title (for add)" },
                    "id": { "type": "integer", "description": "Todo ID (for update)" },
                    "status": {
                        "type": "string",
                        "enum": ["pending", "in_progress", "completed", "cancelled"],
                        "description": "New status (for update)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let action = input["action"].as_str().unwrap_or("list");

        match action {
            "add" => {
                let title = match input["title"].as_str() {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => return Ok(ToolOutput::error("missing 'title' for add")),
                };
                let mut todos = self.store.lock().unwrap();
                let id = todos.len() as u32 + 1;
                todos.push(flint_agent::todo::TodoItem {
                    id,
                    title: title.clone(),
                    status: flint_agent::todo::TodoStatus::Pending,
                });
                Ok(ToolOutput::text(format!("Added todo #{}: {}", id, title)))
            }
            "update" => {
                let id = match input["id"].as_u64() {
                    Some(i) => i as u32,
                    None => return Ok(ToolOutput::error("missing 'id' for update")),
                };
                let status_str = match input["status"].as_str() {
                    Some(s) => s,
                    None => return Ok(ToolOutput::error("missing 'status' for update")),
                };
                let status = match status_str {
                    "pending" => flint_agent::todo::TodoStatus::Pending,
                    "in_progress" => flint_agent::todo::TodoStatus::InProgress,
                    "completed" => flint_agent::todo::TodoStatus::Completed,
                    "cancelled" => flint_agent::todo::TodoStatus::Cancelled,
                    _ => return Ok(ToolOutput::error(format!(
                        "invalid status '{}', use: pending, in_progress, completed, cancelled",
                        status_str
                    ))),
                };
                let mut todos = self.store.lock().unwrap();
                if let Some(item) = todos.iter_mut().find(|t| t.id == id) {
                    let old = item.status.clone();
                    item.status = status.clone();
                    Ok(ToolOutput::text(format!(
                        "Todo #{} '{}': {:?} → {:?}",
                        id, item.title, old, status
                    )))
                } else {
                    Ok(ToolOutput::error(format!("todo #{} not found", id)))
                }
            }
            "list" | _ => {
                let todos = self.store.lock().unwrap();
                if todos.is_empty() {
                    return Ok(ToolOutput::text("No todos.".to_string()));
                }
                let lines: Vec<String> = todos
                    .iter()
                    .map(|t| {
                        let status_icon = match t.status {
                            flint_agent::todo::TodoStatus::Pending => "[ ]",
                            flint_agent::todo::TodoStatus::InProgress => "[~]",
                            flint_agent::todo::TodoStatus::Completed => "[x]",
                            flint_agent::todo::TodoStatus::Cancelled => "[-]",
                        };
                        format!("{} #{} {}", status_icon, t.id, t.title)
                    })
                    .collect();
                Ok(ToolOutput::text(lines.join("\n")))
            }
        }
    }
}

// ── Registration helper ────────────────────────────────────────────────────

/// Register all built-in tools into the registry.
pub fn register_builtins(registry: &mut ToolRegistry) {
    registry.register(ReadTool);
    registry.register(WriteTool);
    registry.register(EditTool);
    registry.register(BashTool);
    registry.register(GrepTool);
    registry.register(GlobTool);
    registry.register(WebFetchTool);
}

/// Register memory tools into the registry (when memory feature is enabled).
pub fn register_memory_tools(registry: &mut ToolRegistry, memory: SharedMemory) {
    registry.register(MemoryRememberTool {
        memory: memory.clone(),
    });
    registry.register(MemoryForgetTool {
        memory: memory.clone(),
    });
    registry.register(MemorySearchTool {
        memory: memory.clone(),
    });
    registry.register(MemoryListTool {
        memory: memory.clone(),
    });
    registry.register(MemoryUpdateCoreTool {
        memory,
    });
}

/// Register the todo tool (when auto-poke is enabled).
pub fn register_todo_tool(registry: &mut ToolRegistry, store: flint_agent::TodoStore) {
    registry.register(TodoTool { store });
}
