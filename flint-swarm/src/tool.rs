//! SwarmTool: coordinator-only task distribution.
//!
//! The coordinator spawns agents and assigns tasks. Agents run independently
//! with full autonomy — the coordinator does NOT manage their tool execution.
//!
//! ## Architecture (mirroring jcode)
//!
//! ```text
//! Coordinator (main agent)
//!     │ swarm spawn → creates agent, returns agent_id
//!     │ swarm assign → assigns task to agent
//!     │ swarm wait → waits for agent to complete
//!     ▼
//! Sub-Agent (independent tokio task)
//!     ├─ Own LLM loop (full autonomy)
//!     ├─ All tools: read, write, edit, bash, grep, glob, web_fetch
//!     ├─ request_input → asks user questions
//!     └─ Reports result via notification channel
//! ```

use anyhow::Result;
use async_trait::async_trait;
use flint_agent::{Tool, ToolContext, ToolRegistry};
use flint_types::{ToolDefinition, ToolOutput};
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

use crate::manager::SwarmManager;
use crate::router::MessageRouter;

type SharedSwarm = Arc<Mutex<SwarmManager>>;

/// Closure that builds a provider for a given model name.
/// Returns None on failure (caller should fall back to default).
pub type ProviderFactory = Box<dyn Fn(&str) -> Option<Arc<dyn flint_provider::Provider>> + Send + Sync>;

pub struct SwarmTool {
    pub swarm: SharedSwarm,
    pub router: Option<Arc<MessageRouter>>,
    pub default_spawn_mode: String,
    /// Default model for sub-agents (from config). None = inherit parent.
    pub default_model: Option<String>,
    /// Per-slot model assignments. Index 0 = agent 1, etc.
    pub agent_models: Vec<String>,
    /// Factory to build providers for model overrides.
    pub build_provider: ProviderFactory,
    /// Model selection strategy: "auto", "slots", or "fixed".
    pub model_selection: String,
    /// Auto-incrementing slot counter for "slots" mode.
    pub next_slot: AtomicUsize,
}

#[async_trait]
impl Tool for SwarmTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "swarm".into(),
            description: "Spawn and manage sub-agents for parallel work. \
                Sub-agents run independently with full autonomy — all file, \
                shell, and communication tools are available to them. \
                The coordinator only distributes tasks and collects results. \
                Commands: spawn, assign, wait, status, stop."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "spawn: create agent with task. status: check agents. stop: stop agent.",
                        "enum": ["spawn", "status", "stop"]
                    },
                    "prompt": { "type": "string", "description": "Task description (for spawn/assign)" },
                    "agent_id": { "type": "string", "description": "Agent ID (for assign/wait/stop)" },
                    "timeout": { "type": "integer", "description": "Timeout in seconds for wait command (default 600)" },
                    "mode": { "type": "string", "description": "terminal (new terminal with full REPL), interactive (streaming display in main terminal), or in-process (background task). Default comes from config.", "enum": ["terminal", "interactive", "in-process"] },
                    "full_context": { "type": "boolean", "description": "For terminal mode: inherit full conversation history (default false, inherits only system prompt + task)", "default": false },
                    "model": { "type": "string", "description": "Override model (only in 'auto' mode). In 'slots' mode this is ignored — models are assigned automatically." },
                    "slot": { "type": "integer", "description": "Agent slot number (1-indexed). In 'slots' mode, models are auto-assigned if omitted. In 'auto' mode, optionally picks a pre-configured slot model." }
                },
                "required": ["command"]
            }),
        }
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(600))
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let command = input["command"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'command'"))?;

        match command {
            // ── Spawn: create a new agent with an initial task ──────────
            // Default: interactive mode (new CMD window with full REPL).
            // The agent runs independently with its own terminal.
            "spawn" => {
                let prompt = input["prompt"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'prompt'"))?;
                let mode = input["mode"].as_str().unwrap_or(&self.default_spawn_mode);

                // Resolve model based on selection strategy
                let resolved_model = match self.model_selection.as_str() {
                    "fixed" => {
                        if input["model"].as_str().is_some() || input["slot"].as_i64().is_some() {
                            return Ok(ToolOutput::error(
                                "model_selection is 'fixed' — cannot override model per-agent. \
                                 Change [features.swarm] model_selection to 'auto' or 'slots' to allow overrides."
                            ));
                        }
                        self.default_model.clone()
                    }
                    "slots" => {
                        if self.agent_models.is_empty() {
                            self.default_model.clone()
                        } else if let Some(slot) = input["slot"].as_i64() {
                            // Explicit slot specified (model= is silently ignored in slots mode)
                            let idx = (slot - 1) as usize; // 1-indexed to 0-indexed
                            if idx >= self.agent_models.len() {
                                return Ok(ToolOutput::error(format!(
                                    "slot {} out of range (have {} slots configured)",
                                    slot, self.agent_models.len()
                                )));
                            }
                            let m = self.agent_models[idx].clone();
                            if m.is_empty() { None } else { Some(m) }
                        } else {
                            // Auto-assign next slot in round-robin
                            let slot_num = self.next_slot.fetch_add(1, Ordering::Relaxed);
                            let idx = slot_num % self.agent_models.len();
                            let m = self.agent_models[idx].clone();
                            if m.is_empty() { None } else { Some(m) }
                        }
                    }
                    _ => {
                        // "auto" — agent decides freely
                        if let Some(m) = input["model"].as_str() {
                            Some(m.to_string())
                        } else if let Some(slot) = input["slot"].as_i64() {
                            let idx = (slot - 1) as usize;
                            self.agent_models.get(idx)
                                .map(|m| m.clone())
                                .filter(|m| !m.is_empty())
                        } else {
                            self.default_model.clone()
                        }
                    }
                };

                // Build provider override if a specific model was resolved
                let provider_override = resolved_model.as_ref().and_then(|m| {
                    (self.build_provider)(m)
                });
                let model_label = resolved_model.clone().unwrap_or_else(|| "default".to_string());

                if mode == "in-process" {
                    // In-process mode: runs as background tokio task
                    let spawn_result = {
                        let mut swarm = self.swarm.lock().unwrap();
                        swarm.spawn_agent(prompt.to_string(), provider_override)
                    };
                    match spawn_result {
                        Ok(result) => {
                            Ok(ToolOutput::text(format!(
                                "Spawned agent {} (task {}) [model: {}]\n\
                                 The agent is running in the background.\n\
                                 Use 'swarm wait agent_id={}' to get its result.",
                                result.agent_id, result.task_id, model_label, result.agent_id,
                            )))
                        }
                        Err(e) => Ok(ToolOutput::error(format!("spawn failed: {}", e))),
                    }
                } else if mode == "terminal" {
                    // Terminal mode: new terminal with full REPL
                    let full_context = input["full_context"].as_bool().unwrap_or(false);
                    let spawn_result = {
                        let mut swarm = self.swarm.lock().unwrap();
                        swarm.spawn_terminal(
                            prompt.to_string(),
                            None, // conversation history not available from tool input
                            full_context,
                            resolved_model, // terminal sub-agent picks up model via SpawnContext
                        )
                    };
                    match spawn_result {
                        Ok(result) => {
                            Ok(ToolOutput::text(format!(
                                "Spawned terminal agent [{}] in new window [model: {}].\n\
                                 Task ID: {}\n\
                                 The agent is running as an independent REPL with its own terminal.\n\
                                 It communicates with you via the MessageRouter.\n\
                                 Use 'swarm wait agent_id={}' to retrieve the result.",
                                &result.agent_id[result.agent_id.len()-4..],
                                model_label,
                                result.task_id, result.agent_id,
                            )))
                        }
                        Err(e) => Ok(ToolOutput::error(format!("spawn failed: {}", e))),
                    }
                } else {
                    // Interactive mode: streaming display in main terminal
                    let spawn_result = {
                        let mut swarm = self.swarm.lock().unwrap();
                        swarm.spawn_interactive(prompt.to_string(), provider_override)
                    };
                    match spawn_result {
                        Ok(agent_id) => {
                            let task_id = {
                                let swarm = self.swarm.lock().unwrap();
                                swarm.agent_status().iter()
                                    .find(|(id, _, _)| id == &agent_id)
                                    .and_then(|(_, _, tid)| tid.clone())
                                    .unwrap_or_default()
                            };
                            let result_dir = crate::log::log_dir();
                            Ok(ToolOutput::text(format!(
                                "Spawned interactive agent [{}] [model: {}].\n\
                                 Task ID: {}\n\
                                 The agent is running independently with its own REPL.\n\
                                 It will save results to: {}\\*_{}.result.md\n\
                                 Use 'swarm wait agent_id={}' to retrieve the result.",
                                &agent_id[agent_id.len()-4..], model_label, task_id,
                                result_dir.display(), task_id, agent_id,
                            )))
                        }
                        Err(e) => Ok(ToolOutput::error(format!("spawn failed: {}", e))),
                    }
                }
            }

            // ── Assign: give a task to an existing agent (blocks until done) ──
            // The agent has its own session and tools — this just sends a
            // message and waits for the agent's independent response.
            "assign" => {
                let agent_id = input["agent_id"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'agent_id'"))?;
                let prompt = input["prompt"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'prompt'"))?;

                // Try router first
                if let Some(ref router) = self.router {
                    if router.is_connected(agent_id).await {
                        router.send_to_agent(agent_id, prompt).await
                            .map_err(|e| anyhow::anyhow!("router send failed: {}", e))?;
                        return Ok(ToolOutput::text(format!(
                            "Task assigned to agent {} via router.", agent_id
                        )));
                    }
                }

                // Fallback: in-process channel (blocks for result)
                let result_rx = {
                    let swarm = self.swarm.lock().unwrap();
                    swarm.send_followup(agent_id, prompt.to_string())
                };

                match result_rx {
                    Ok(rx) => {
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(600), rx
                        ).await {
                            Ok(Ok(Ok(text))) => Ok(ToolOutput::text(format!("[{}]\n{}", agent_id, text))),
                            Ok(Ok(Err(e))) => Ok(ToolOutput::error(format!("agent {} failed: {}", agent_id, e))),
                            Ok(Err(_)) => Ok(ToolOutput::error("agent dropped result channel")),
                            Err(_) => Ok(ToolOutput::error(format!(
                                "agent {} did not respond within 600s", agent_id
                            ))),
                        }
                    }
                    Err(e) => Ok(ToolOutput::error(e.to_string())),
                }
            }

            // ── Wait: check if an agent has completed (non-blocking) ────
            "wait" => {
                let agent_id = input["agent_id"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing 'agent_id'"))?;

                // Check cached result (non-blocking)
                let (task_id, cached) = {
                    let swarm = self.swarm.lock().unwrap();
                    let tid = swarm.agent_status().iter()
                        .find(|(id, _, _)| id == agent_id)
                        .and_then(|(_, _, tid)| tid.clone());
                    let cached = tid.as_ref().and_then(|t| swarm.get_task_result(t));
                    (tid, cached)
                };

                if let Some(result) = cached {
                    return Ok(ToolOutput::text(format!("[{}]\n{}", agent_id, result)));
                }

                Ok(ToolOutput::text(format!(
                    "Agent {} is still running. Results will be delivered automatically when complete. \
                     You do not need to wait — continue with other work.",
                    agent_id
                )))
            }

            // ── Status: check all agents ────────────────────────────────
            "status" => {
                let swarm = self.swarm.lock().unwrap();
                let agents = swarm.agent_status();
                let tasks = swarm.task_status();
                let mut out = format!("Swarm: {} active agents, {} tasks\n\n",
                    swarm.active_agent_count(), tasks.len());
                for (id, status, task_id) in &agents {
                    out.push_str(&format!("  {} [{}] -> {}\n", id, status,
                        task_id.as_deref().unwrap_or("?")));
                }
                if !tasks.is_empty() {
                    out.push('\n');
                    for task in &tasks {
                        out.push_str(&format!("  {} [{}]: {}\n", task.id, task.status, task.content));
                    }
                }
                Ok(ToolOutput::text(out))
            }

            // ── Stop: stop an agent ─────────────────────────────────────
            "stop" => {
                let mut swarm = self.swarm.lock().unwrap();
                if let Some(agent_id) = input["agent_id"].as_str() {
                    match swarm.stop_agent(agent_id) {
                        Ok(()) => Ok(ToolOutput::text(format!("stopped agent {}", agent_id))),
                        Err(e) => Ok(ToolOutput::error(e.to_string())),
                    }
                } else {
                    swarm.stop_all();
                    Ok(ToolOutput::text("stopped all agents"))
                }
            }

            _ => Ok(ToolOutput::error(format!(
                "unknown command: '{}'. Use: spawn, assign, wait, status, stop",
                command
            ))),
        }
    }
}

// ── Sub-agent tools (file/shell — full autonomy) ──────────────────────

pub struct SubReadTool;
#[async_trait]
impl Tool for SubReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "read".into(), description: "Read a file.".into(),
            parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}) }
    }
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = input["path"].as_str().ok_or_else(|| anyhow::anyhow!("missing required parameter 'path'. Usage: read(path=\"file.txt\")"))?;
        let full = ctx.working_dir.join(path);
        match std::fs::read_to_string(&full) {
            Ok(c) => Ok(ToolOutput::text(c)),
            Err(e) => Ok(ToolOutput::error(format!("{}: {}", full.display(), e))),
        }
    }
}

pub struct SubWriteTool;
#[async_trait]
impl Tool for SubWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "write".into(), description: "Write to a file.".into(),
            parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}) }
    }
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = input["path"].as_str().ok_or_else(|| anyhow::anyhow!("missing required parameter 'path'. Usage: write(path=\"file.txt\", content=\"...\")"))?;
        let content = input["content"].as_str().ok_or_else(|| anyhow::anyhow!("missing required parameter 'content'. Usage: write(path=\"file.txt\", content=\"...\")"))?;
        let full = ctx.working_dir.join(path);
        if let Some(p) = full.parent() { std::fs::create_dir_all(p)?; }
        std::fs::write(&full, content)?;
        Ok(ToolOutput::text(format!("wrote {}", full.display())))
    }
}

pub struct SubEditTool;
#[async_trait]
impl Tool for SubEditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "edit".into(), description: "Replace text in a file.".into(),
            parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}},"required":["path","old_string","new_string"]}) }
    }
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = input["path"].as_str().ok_or_else(|| anyhow::anyhow!("missing required parameter 'path'. Usage: edit(path=\"file.txt\", old_string=\"...\", new_string=\"...\")"))?;
        let old = input["old_string"].as_str().ok_or_else(|| anyhow::anyhow!("missing required parameter 'old_string'"))?;
        let new = input["new_string"].as_str().ok_or_else(|| anyhow::anyhow!("missing required parameter 'new_string'"))?;
        let all = input["replace_all"].as_bool().unwrap_or(false);
        if old.is_empty() { return Ok(ToolOutput::error("old_string cannot be empty")); }
        let full = ctx.working_dir.join(path);
        let content = std::fs::read_to_string(&full).map_err(|e| anyhow::anyhow!("{}: {}", full.display(), e))?;
        let count = content.matches(old).count();
        if count == 0 { return Ok(ToolOutput::error(format!("old_string not found in {}", full.display()))); }
        if !all && count > 1 { return Ok(ToolOutput::error(format!("old_string found {} times. Use replace_all=true.", count))); }
        let new_content = if all { content.replace(old, new) } else { content.replacen(old, new, 1) };
        std::fs::write(&full, &new_content)?;
        Ok(ToolOutput::text(format!("edited {}", full.display())))
    }
}

pub struct SubBashTool;
#[async_trait]
impl Tool for SubBashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "bash".into(), description: "Run a shell command.".into(),
            parameters: serde_json::json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}) }
    }
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let cmd = input["command"].as_str().ok_or_else(|| anyhow::anyhow!("missing required parameter 'command'. Usage: bash(command=\"ls -la\")"))?;
        let output = if cfg!(target_os = "windows") {
            std::process::Command::new("cmd").args(["/C", &format!("chcp 65001 >nul && {}", cmd)])
                .current_dir(&ctx.working_dir).output()?
        } else {
            std::process::Command::new("sh").args(["-c", cmd]).current_dir(&ctx.working_dir).output()?
        };
        let combined = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        if output.status.success() { Ok(ToolOutput::text(combined)) }
        else { Ok(ToolOutput::error(format!("exit code: {}\n{}", output.status.code().unwrap_or(-1), combined))) }
    }
}

pub struct SubGrepTool;
#[async_trait]
impl Tool for SubGrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "grep".into(), description: "Search for a pattern in files.".into(),
            parameters: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}) }
    }
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let pattern = input["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'pattern'"))?;
        let path = input["path"].as_str().unwrap_or(".");
        let full = ctx.working_dir.join(path);
        match std::process::Command::new("rg").args(["--no-heading", "-n", pattern, full.to_str().unwrap_or(".")]).output() {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                Ok(ToolOutput::text(if text.is_empty() { "no matches found".into() } else { text.to_string() }))
            }
            Err(_) => Ok(ToolOutput::error("ripgrep (rg) not found in PATH")),
        }
    }
}

pub struct SubGlobTool;
#[async_trait]
impl Tool for SubGlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "glob".into(), description: "Find files matching a glob pattern.".into(),
            parameters: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"}},"required":["pattern"]}) }
    }
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let pattern = input["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'pattern'"))?;
        let full = ctx.working_dir.join(pattern);
        let paths: Vec<String> = glob::glob(&full.to_string_lossy())
            .map_err(|e| anyhow::anyhow!("invalid glob: {}", e))?
            .filter_map(|p| p.ok()).map(|p| p.to_string_lossy().to_string()).collect();
        Ok(ToolOutput::text(if paths.is_empty() { "no files matched".into() } else { paths.join("\n") }))
    }
}

pub struct SubWebFetchTool;
#[async_trait]
impl Tool for SubWebFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "web_fetch".into(), description: "Fetch a URL.".into(),
            parameters: serde_json::json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}) }
    }
    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let url = input["url"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'url'"))?;
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5)).build()?;
        let resp = client.get(url).send().await?;
        if !resp.status().is_success() {
            return Ok(ToolOutput::error(format!("HTTP {} for {}", resp.status().as_u16(), url)));
        }
        let body = resp.text().await?;
        let max = 50000;
        Ok(ToolOutput::text(if body.len() > max { format!("{}...\n\n[truncated, {} chars]", &body[..max], body.len()) } else { body }))
    }
}

// ── Agent-to-agent communication tools ───────────────────────────────────

pub struct AgentSendTool {
    pub router: Arc<MessageRouter>,
    pub agent_id: String,
}

#[async_trait]
impl Tool for AgentSendTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "send_to_agent".into(),
            description: "Send a message to another swarm agent.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": { "type": "string" },
                    "message": { "type": "string" }
                },
                "required": ["agent_id", "message"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let to = input["agent_id"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'agent_id'"))?;
        let message = input["message"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'message'"))?;
        self.router.send_to_agent(to, message).await
            .map_err(|e| anyhow::anyhow!("send failed: {}", e))?;
        Ok(ToolOutput::text(format!("message sent to {}", to)))
    }
}

pub struct AgentListTool {
    pub router: Arc<MessageRouter>,
}

#[async_trait]
impl Tool for AgentListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_agents".into(),
            description: "List all swarm agents currently connected to the router.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, _input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let agents = self.router.list_agents().await;
        if agents.is_empty() {
            Ok(ToolOutput::text("no agents connected".to_string()))
        } else {
            Ok(ToolOutput::text(format!("{} connected agent(s):\n{}", agents.len(), agents.join("\n"))))
        }
    }
}

// ── RequestInputTool: agent asks user a question ──────────────────────

pub struct RequestInputTool {
    pub agent_id: String,
    pub request_tx: tokio::sync::mpsc::Sender<crate::agent::InputRequest>,
}

/// Sentinel prefix in tool output indicating an input request was sent.
pub const INPUT_REQUESTED_PREFIX: &str = "[INPUT_REQUESTED:";

#[async_trait]
impl Tool for RequestInputTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "request_input".into(),
            description: "Ask the user a question. Use when you need clarification \
                or additional information to proceed. The response will be provided \
                automatically — continue your work after receiving it."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "The question to ask" }
                },
                "required": ["prompt"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let prompt = input["prompt"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'prompt'"))?;

        let req = crate::agent::InputRequest {
            agent_id: self.agent_id.clone(),
            prompt: prompt.to_string(),
        };
        self.request_tx.send(req).await
            .map_err(|e| anyhow::anyhow!("failed to send input request: {}", e))?;

        Ok(ToolOutput::text(format!("{}{}]", INPUT_REQUESTED_PREFIX, prompt)))
    }
}

// ── Registration ─────────────────────────────────────────────────────────

pub fn register_swarm_tools(
    registry: &mut ToolRegistry,
    swarm: SharedSwarm,
    router: Option<Arc<MessageRouter>>,
    default_spawn_mode: String,
    default_model: Option<String>,
    agent_models: Vec<String>,
    build_provider: ProviderFactory,
    model_selection: String,
) {
    registry.register(SwarmTool {
        swarm,
        router,
        default_spawn_mode,
        default_model,
        agent_models,
        build_provider,
        model_selection,
        next_slot: AtomicUsize::new(0),
    });
}

/// Register base file/shell tools on a sub-agent's registry.
/// These give the sub-agent full autonomy to read, write, edit files,
/// run shell commands, search code, and fetch URLs.
pub fn register_sub_agent_tools(registry: &mut ToolRegistry) {
    registry.register(SubReadTool);
    registry.register(SubWriteTool);
    registry.register(SubEditTool);
    registry.register(SubBashTool);
    registry.register(SubGrepTool);
    registry.register(SubGlobTool);
    registry.register(SubWebFetchTool);
}

pub fn register_agent_comm_tools(registry: &mut ToolRegistry, router: Arc<MessageRouter>, agent_id: &str) {
    registry.register(AgentSendTool { router: router.clone(), agent_id: agent_id.to_string() });
    registry.register(AgentListTool { router });
}

pub fn register_input_tool(
    registry: &mut ToolRegistry,
    request_tx: tokio::sync::mpsc::Sender<crate::agent::InputRequest>,
    agent_id: &str,
) {
    registry.register(RequestInputTool {
        agent_id: agent_id.to_string(),
        request_tx,
    });
}
