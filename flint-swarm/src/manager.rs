//! SwarmManager: task registry and agent lifecycle.
//!
//! Sub-agents stay alive after completing tasks, waiting for follow-up
//! messages. Session context is preserved across turns.

use crate::agent::{AgentRequest, InputRequest, InputResponse};
use crate::log;
use crate::output::OutputSender;
use crate::router::{AgentResult, MessageRouter};
use crate::types::{AgentNotification, AgentStatus, SwarmConfig, TaskItem, TaskStatus};
use flint_agent::{ToolContext, ToolRegistry};
use flint_provider::Provider;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

struct AgentHandle {
    status: AgentStatus,
    task_id: String,
    request_tx: mpsc::Sender<AgentRequest>,
    /// Whether this is an interactive agent.
    is_interactive: bool,
    /// Stored result receiver from the initial spawn.
    /// Consumed by the first `wait` call.
    initial_result_rx: std::sync::Mutex<Option<oneshot::Receiver<Result<String, String>>>>,
    /// Channel to send input responses to the sub-agent's request_input tool.
    input_response_tx: Option<mpsc::Sender<InputResponse>>,
    /// Channel to receive input requests from the sub-agent.
    input_request_rx: std::sync::Mutex<Option<mpsc::Receiver<InputRequest>>>,
    /// Streaming output channel (for interactive agents).
    /// The REPL polls this to display agent output in real-time.
    stream_rx: std::sync::Mutex<Option<mpsc::Receiver<String>>>,
    _join: tokio::task::JoinHandle<()>,
}

pub struct SpawnResult {
    pub agent_id: String,
    pub task_id: String,
}

pub struct SwarmManager {
    tasks: HashMap<String, TaskItem>,
    agents: HashMap<String, AgentHandle>,
    config: SwarmConfig,
    provider: Arc<dyn Provider>,
    working_dir: std::path::PathBuf,
    system_prompt: String,
    output_tx: OutputSender,
    /// The coordinator's full tool registry (cloned for sub-agents).
    /// This gives sub-agents access to all tools including swarm, memory, etc.
    registry: ToolRegistry,
    /// TCP-based message router for real-time agent communication.
    router: Option<Arc<MessageRouter>>,
    /// Channel for sub-agent completion notifications.
    /// The REPL drains this between turns to inform the main agent.
    notify_tx: mpsc::Sender<AgentNotification>,
    notify_rx: Option<mpsc::Receiver<AgentNotification>>,
}

impl SwarmManager {
    pub fn new(
        config: SwarmConfig,
        provider: Arc<dyn Provider>,
        working_dir: std::path::PathBuf,
        system_prompt: String,
        output_tx: OutputSender,
        registry: ToolRegistry,
        router: Option<Arc<MessageRouter>>,
    ) -> Self {
        let (notify_tx, notify_rx) = mpsc::channel(64);
        Self {
            tasks: HashMap::new(),
            agents: HashMap::new(),
            config,
            provider,
            working_dir,
            system_prompt,
            output_tx,
            registry,
            router,
            notify_tx,
            notify_rx: Some(notify_rx),
        }
    }

    /// Take the notification receiver. Can only be called once.
    /// The REPL uses this to receive sub-agent completion events.
    pub fn take_notify_rx(&mut self) -> Option<mpsc::Receiver<AgentNotification>> {
        self.notify_rx.take()
    }

    /// Spawn a sub-agent. Returns a oneshot receiver for the first result.
    /// The agent stays alive for follow-up messages.
    /// The result is also delivered via the notification channel.
    pub fn spawn_agent(&mut self, prompt: String) -> anyhow::Result<SpawnResult> {
        if self.agents.len() >= self.config.max_agents {
            return Err(anyhow::anyhow!(
                "max agents ({}) reached. Stop an agent first.",
                self.config.max_agents
            ));
        }

        let agent_id = format!("agent_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let task_id = format!("task_{}", &uuid::Uuid::new_v4().to_string()[..8]);

        self.tasks.insert(task_id.clone(), TaskItem {
            id: task_id.clone(),
            content: prompt.clone(),
            status: TaskStatus::Running,
            assigned_to: Some(agent_id.clone()),
            result: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
        });

        // Create log file + viewer
        let (_log_file, _log_path) = log::create_log(&agent_id, &task_id);
        if self.config.open_viewer {
            log::open_agent_viewer(&agent_id, &task_id);
        }

        // Create channels
        let (request_tx, request_rx) = mpsc::channel(16);
        let (result_tx, result_rx) = oneshot::channel();

        // Create input channels for interactive sub-agent communication
        let (input_request_tx, input_request_rx) = mpsc::channel::<InputRequest>(4);
        let (input_response_tx, input_response_rx) = mpsc::channel::<InputResponse>(4);

        // Send initial prompt through the channel
        let init_tx = request_tx.clone();
        tokio::spawn(async move {
            let _ = init_tx.send(AgentRequest::Execute { prompt, result_tx }).await;
        });

        // Spawn agent task (stays alive waiting for messages)
        // Clone the coordinator's full registry so the sub-agent has all tools.
        let provider = self.provider.clone();
        let system = self.system_prompt.clone();
        let ctx = ToolContext { working_dir: self.working_dir.clone() };
        let output_tx = self.output_tx.clone();
        let notify_tx = self.notify_tx.clone();
        let registry = self.registry.clone();
        let router = self.router.clone();
        let max_turns = self.config.agent_max_turns;
        let max_output = self.config.max_output_chars;
        let aid = agent_id.clone();
        let tid = task_id.clone();

        let join = tokio::spawn(async move {
            crate::agent::run_sub_agent(
                aid, tid, provider, system, ctx,
                max_turns, max_output, output_tx, notify_tx, request_rx, registry,
                router,
                Some(input_request_tx),
                Some(input_response_rx),
                None, // no display client
                None, // no stream
            ).await;
        });

        self.agents.insert(agent_id.clone(), AgentHandle {
            status: AgentStatus::Running,
            task_id: task_id.clone(),
            request_tx,
            is_interactive: false,
            initial_result_rx: std::sync::Mutex::new(Some(result_rx)),
            input_response_tx: Some(input_response_tx),
            input_request_rx: std::sync::Mutex::new(Some(input_request_rx)),
            stream_rx: std::sync::Mutex::new(None),
            _join: join,
        });

        Ok(SpawnResult { agent_id, task_id })
    }

    /// Spawn an interactive sub-agent that streams output to the main REPL.
    ///
    /// The agent runs as a tokio task. Its output is streamed to the main
    /// terminal via a channel. The REPL displays the output and forwards
    /// user input to the agent when needed.
    pub fn spawn_interactive(&mut self, prompt: String) -> anyhow::Result<String> {
        if self.agents.len() >= self.config.max_agents {
            return Err(anyhow::anyhow!("max agents ({}) reached", self.config.max_agents));
        }

        let agent_id = format!("agent_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let task_id = format!("task_{}", &uuid::Uuid::new_v4().to_string()[..8]);

        self.tasks.insert(task_id.clone(), TaskItem {
            id: task_id.clone(),
            content: prompt.clone(),
            status: TaskStatus::Running,
            assigned_to: Some(agent_id.clone()),
            result: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
        });

        // Create channels
        let (request_tx, request_rx) = mpsc::channel(16);
        let (result_tx, result_rx) = oneshot::channel();
        let (input_request_tx, input_request_rx) = mpsc::channel::<InputRequest>(4);
        let (input_response_tx, input_response_rx) = mpsc::channel::<InputResponse>(4);
        // Streaming output channel — REPL polls this to display agent output
        let (stream_tx, stream_rx) = mpsc::channel::<String>(256);

        // Send initial prompt through the channel
        let init_tx = request_tx.clone();
        tokio::spawn(async move {
            let _ = init_tx.send(AgentRequest::Execute { prompt, result_tx }).await;
        });

        // Spawn agent task with streaming output
        let provider = self.provider.clone();
        let system = self.system_prompt.clone();
        let ctx = ToolContext { working_dir: self.working_dir.clone() };
        let output_tx = self.output_tx.clone();
        let notify_tx = self.notify_tx.clone();
        let registry = self.registry.clone();
        let router = self.router.clone();
        let max_turns = self.config.agent_max_turns;
        let max_output = self.config.max_output_chars;
        let aid = agent_id.clone();
        let tid = task_id.clone();

        let join = tokio::spawn(async move {
            crate::agent::run_sub_agent(
                aid, tid, provider, system, ctx,
                max_turns, max_output, output_tx, notify_tx, request_rx, registry,
                router,
                Some(input_request_tx),
                Some(input_response_rx),
                None, // no display client (output goes to stream)
                Some(stream_tx),
            ).await;
        });

        self.agents.insert(agent_id.clone(), AgentHandle {
            status: AgentStatus::Running,
            task_id: task_id.clone(),
            request_tx,
            is_interactive: true,
            initial_result_rx: std::sync::Mutex::new(Some(result_rx)),
            input_response_tx: Some(input_response_tx),
            input_request_rx: std::sync::Mutex::new(Some(input_request_rx)),
            stream_rx: std::sync::Mutex::new(Some(stream_rx)),
            _join: join,
        });

        Ok(agent_id)
    }

    /// Spawn a full sub-agent REPL in a new terminal (方案 A).
    ///
    /// The sub-agent runs as an independent `flint` process with its own
    /// terminal, Session, and LLM calls. It communicates with the coordinator
    /// via the TCP MessageRouter.
    pub fn spawn_terminal(
        &mut self,
        prompt: String,
        conversation_history: Option<Vec<flint_types::Message>>,
        full_context: bool,
    ) -> anyhow::Result<SpawnResult> {
        if self.agents.len() >= self.config.max_agents {
            return Err(anyhow::anyhow!(
                "max agents ({}) reached. Stop an agent first.",
                self.config.max_agents
            ));
        }

        let agent_id = format!("agent_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let task_id = format!("task_{}", &uuid::Uuid::new_v4().to_string()[..8]);

        self.tasks.insert(task_id.clone(), TaskItem {
            id: task_id.clone(),
            content: prompt.clone(),
            status: TaskStatus::Running,
            assigned_to: Some(agent_id.clone()),
            result: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
        });

        let router_addr = self.router.as_ref()
            .map(|r| r.addr.to_string())
            .unwrap_or_default();

        // Build core memory content (if available from system prompt)
        let core_memory = ""; // Core memory is already embedded in system_prompt

        crate::agent::spawn_terminal_agent(
            &agent_id,
            &task_id,
            &prompt,
            &self.system_prompt,
            conversation_history,
            core_memory,
            &self.working_dir,
            &router_addr,
            full_context,
        )?;

        // Create a dummy join handle — the external process is not a tokio task.
        // We monitor it via the MessageRouter connection instead.
        let join = tokio::spawn(async move {
            // Wait indefinitely — the external process manages its own lifecycle.
            // This task exists only to keep the AgentHandle alive.
            futures::future::pending::<()>().await;
        });

        // Create channels for potential follow-up via router
        let (request_tx, _request_rx) = tokio::sync::mpsc::channel(16);

        self.agents.insert(agent_id.clone(), AgentHandle {
            status: AgentStatus::Running,
            task_id: task_id.clone(),
            request_tx,
            is_interactive: true,
            initial_result_rx: std::sync::Mutex::new(None),
            input_response_tx: None,
            input_request_rx: std::sync::Mutex::new(None),
            stream_rx: std::sync::Mutex::new(None),
            _join: join,
        });

        Ok(SpawnResult { agent_id, task_id })
    }

    /// Send a follow-up message to an existing agent.
    /// Returns a oneshot receiver for the result.
    pub fn send_followup(&self, agent_id: &str, prompt: String) -> anyhow::Result<oneshot::Receiver<Result<String, String>>> {
        let handle = self.agents.get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("agent '{}' not found", agent_id))?;

        if handle.status != AgentStatus::Running && handle.status != AgentStatus::Completed {
            return Err(anyhow::anyhow!("agent '{}' is not active (status: {})", agent_id, handle.status));
        }

        let (result_tx, result_rx) = oneshot::channel();
        let tx = handle.request_tx.clone();

        tokio::spawn(async move {
            let _ = tx.send(AgentRequest::Execute { prompt, result_tx }).await;
        });

        Ok(result_rx)
    }

    /// Check if an agent is still alive and can receive messages.
    pub fn is_agent_alive(&self, agent_id: &str) -> bool {
        self.agents.get(agent_id)
            .map(|h| h.status != AgentStatus::Stopped)
            .unwrap_or(false)
    }

    /// Check if an agent is interactive (spawned in a new terminal).
    pub fn is_interactive(&self, agent_id: &str) -> bool {
        self.agents.get(agent_id)
            .map(|h| h.is_interactive)
            .unwrap_or(false)
    }

    /// Get the task_id for an agent.
    pub fn agent_task_id(&self, agent_id: &str) -> Option<String> {
        self.agents.get(agent_id).map(|h| h.task_id.clone())
    }

    /// Get the router address (if router is running).
    pub fn router_addr(&self) -> Option<std::net::SocketAddr> {
        self.router.as_ref().map(|r| r.addr)
    }

    /// Send a message to an agent through the router.
    pub async fn send_via_router(&self, agent_id: &str, content: &str) -> Result<(), String> {
        if let Some(ref router) = self.router {
            router.send_to_agent(agent_id, content).await
                .map_err(|e| e.to_string())
        } else {
            Err("no router available".to_string())
        }
    }

    /// Get a clone of the router Arc (for direct access from REPL).
    pub fn router_arc(&self) -> Option<Arc<MessageRouter>> {
        self.router.clone()
    }

    /// Drain results from the router.
    pub async fn drain_router_results(&self) -> Vec<AgentResult> {
        if let Some(ref router) = self.router {
            router.drain_results().await
        } else {
            Vec::new()
        }
    }

    /// Drain pending results from the router and return formatted messages
    /// for injection into the coordinator's conversation.
    /// Also updates task status for each received result.
    pub async fn collect_pending_results(&mut self) -> Vec<(String, String)> {
        let results = self.drain_router_results().await;
        let mut messages = Vec::new();
        for r in results {
            // Update task status
            self.complete_task(&r.task_id, &r.result, true);
            let short_id = &r.agent_id[r.agent_id.len().min(7)..];
            let msg = format!(
                "[Sub-agent {} completed task {}]\n{}",
                short_id, &r.task_id[..8.min(r.task_id.len())], r.result
            );
            messages.push((r.agent_id, msg));
        }
        messages
    }

    /// Take the initial result receiver for an agent.
    /// Returns None if already taken (second wait call) or agent not found.
    pub fn take_initial_result(&self, agent_id: &str) -> Option<oneshot::Receiver<Result<String, String>>> {
        self.agents.get(agent_id)
            .and_then(|h| h.initial_result_rx.lock().unwrap().take())
    }

    /// Check if a task has completed (poll-based).
    pub fn is_task_completed(&self, task_id: &str) -> bool {
        self.tasks.get(task_id)
            .map(|t| t.status == TaskStatus::Completed || t.status == TaskStatus::Failed)
            .unwrap_or(false)
    }

    /// Wait for a specific agent's result via the TCP Router.
    /// Polls the router's result channel until a matching result arrives or timeout.
    /// This replaces file-based polling — all result delivery goes through the router.
    pub async fn wait_result(&self, agent_id: &str, timeout: std::time::Duration) -> Result<String, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        let router = self.router.as_ref()
            .ok_or_else(|| "no router available".to_string())?;

        loop {
            // Drain all pending results from the router
            let results = router.drain_results().await;
            for r in results {
                if r.agent_id == agent_id {
                    // Also update task status
                    // (can't call complete_task here because we have &self not &mut self)
                    return Ok(r.result);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!("agent {} did not respond within {:?}", agent_id, timeout));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Update task status from agent events.
    /// Agent stays alive for follow-ups even after task completion.
    pub fn complete_task(&mut self, task_id: &str, result: &str, success: bool) {
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.status = if success { TaskStatus::Completed } else { TaskStatus::Failed };
            task.result = Some(result.to_string());
            task.completed_at = Some(chrono::Utc::now().to_rfc3339());
        }
        // Don't change agent status — it stays alive for follow-ups
        // The agent_status tracks the task result, not whether the agent is alive
    }

    pub fn stop_agent(&mut self, agent_id: &str) -> anyhow::Result<()> {
        if let Some(handle) = self.agents.get_mut(agent_id) {
            let tx = handle.request_tx.clone();
            handle.status = AgentStatus::Stopped;
            tokio::spawn(async move {
                let _ = tx.send(AgentRequest::Stop).await;
            });
            if let Some(task) = self.tasks.get_mut(&handle.task_id.clone()) {
                task.status = TaskStatus::Failed;
                task.result = Some("stopped".to_string());
            }
            Ok(())
        } else {
            Err(anyhow::anyhow!("agent '{}' not found", agent_id))
        }
    }

    pub fn stop_all(&mut self) {
        let ids: Vec<String> = self.agents.keys().cloned().collect();
        for id in ids { let _ = self.stop_agent(&id); }
    }

    pub fn agent_status(&self) -> Vec<(String, AgentStatus, Option<String>)> {
        self.agents.iter()
            .map(|(id, h)| (id.clone(), h.status.clone(), Some(h.task_id.clone())))
            .collect()
    }

    pub fn task_status(&self) -> Vec<&TaskItem> {
        self.tasks.values().collect()
    }

    pub fn get_task_result(&self, task_id: &str) -> Option<String> {
        self.tasks.get(task_id).and_then(|t| t.result.clone())
    }

    /// Count agents that are still alive (can receive follow-ups).
    /// All spawned agents stay alive until explicitly stopped.
    pub fn active_agent_count(&self) -> usize {
        self.agents.values()
            .filter(|h| h.status != AgentStatus::Stopped && h.status != AgentStatus::Failed)
            .count()
    }

    pub fn config(&self) -> &SwarmConfig {
        &self.config
    }

    /// Drain all pending input requests from sub-agents.
    /// Returns a list of (agent_id, prompt) pairs.
    pub fn drain_input_requests(&self) -> Vec<InputRequest> {
        let mut requests = Vec::new();
        for (_id, handle) in &self.agents {
            if let Ok(mut rx_guard) = handle.input_request_rx.try_lock() {
                if let Some(ref mut rx) = *rx_guard {
                    while let Ok(req) = rx.try_recv() {
                        requests.push(req);
                    }
                }
            }
        }
        requests
    }

    /// Send an input response to a specific agent.
    pub async fn send_input_response(&self, agent_id: &str, text: String) -> anyhow::Result<()> {
        let handle = self.agents.get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("agent '{}' not found", agent_id))?;
        if let Some(ref tx) = handle.input_response_tx {
            tx.send(InputResponse { text }).await
                .map_err(|e| anyhow::anyhow!("failed to send input response: {}", e))?;
            Ok(())
        } else {
            Err(anyhow::anyhow!("agent '{}' does not support input", agent_id))
        }
    }

    /// Check if any agent has pending input requests.
    pub fn has_pending_input_requests(&self) -> bool {
        for (_id, handle) in &self.agents {
            if let Ok(rx_guard) = handle.input_request_rx.try_lock() {
                if let Some(ref rx) = *rx_guard {
                    if !rx.is_empty() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Get a clone of the input response sender for a specific agent.
    /// Used by the swarm tool's stdin command.
    pub fn get_input_response_tx(&self, agent_id: &str) -> Option<mpsc::Sender<InputResponse>> {
        self.agents.get(agent_id)
            .and_then(|h| h.input_response_tx.clone())
    }

    /// Drain streaming output from a specific agent.
    /// Returns all pending text chunks.
    pub fn drain_stream(&self, agent_id: &str) -> Vec<String> {
        let mut chunks = Vec::new();
        if let Some(handle) = self.agents.get(agent_id) {
            if let Ok(mut rx_guard) = handle.stream_rx.try_lock() {
                if let Some(ref mut rx) = *rx_guard {
                    while let Ok(chunk) = rx.try_recv() {
                        chunks.push(chunk);
                    }
                }
            }
        }
        chunks
    }

    /// Drain streaming output from ALL interactive agents.
    /// Returns (agent_id, chunk) pairs.
    pub fn drain_all_streams(&self) -> Vec<(String, String)> {
        let mut results = Vec::new();
        for (agent_id, handle) in &self.agents {
            if let Ok(mut rx_guard) = handle.stream_rx.try_lock() {
                if let Some(ref mut rx) = *rx_guard {
                    while let Ok(chunk) = rx.try_recv() {
                        results.push((agent_id.clone(), chunk));
                    }
                }
            }
        }
        results
    }
}
