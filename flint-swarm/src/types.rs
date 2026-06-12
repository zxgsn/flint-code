//! Core types for the swarm coordination system.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Spawning,
    Ready,
    Running,
    Completed,
    Failed,
    Stopped,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Spawning => write!(f, "spawning"),
            AgentStatus::Ready => write!(f, "ready"),
            AgentStatus::Running => write!(f, "running"),
            AgentStatus::Completed => write!(f, "completed"),
            AgentStatus::Failed => write!(f, "failed"),
            AgentStatus::Stopped => write!(f, "stopped"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::Running => write!(f, "running"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: String,
    pub content: String,
    pub status: TaskStatus,
    pub assigned_to: Option<String>,
    pub result: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SwarmConfig {
    pub max_agents: usize,
    pub agent_max_turns: u32,
    pub max_output_chars: usize,
    /// Whether to open viewer terminals for sub-agent logs.
    /// Set to false in tests to avoid spawning terminal windows.
    pub open_viewer: bool,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            max_agents: 5,
            agent_max_turns: 20,
            max_output_chars: 65536,
            open_viewer: true,
        }
    }
}

/// Notification sent when a sub-agent completes its task.
/// Used to inform the main agent REPL of sub-agent results.
#[derive(Debug)]
pub struct AgentNotification {
    pub agent_id: String,
    pub task_id: String,
    pub result: Result<String, String>,
}

/// Context serialized to disk and loaded by a sub-agent spawned in a new terminal.
///
/// When the coordinator spawns a sub-agent in a new terminal, it writes this
/// struct as JSON. The sub-agent's REPL loads it on startup to inherit context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnContext {
    /// Agent ID for router registration.
    pub agent_id: String,
    /// Task ID for tracking.
    pub task_id: String,
    /// Inherited system prompt (includes core memory if available).
    pub system_prompt: String,
    /// Inherited conversation history. None = minimal context mode.
    #[serde(default)]
    pub conversation_history: Option<Vec<flint_types::Message>>,
    /// Core memory content as formatted text.
    #[serde(default)]
    pub core_memory: String,
    /// MessageRouter address (127.0.0.1:port) for coordinator communication.
    pub router_addr: String,
    /// Working directory for the sub-agent.
    pub working_dir: std::path::PathBuf,
    /// Initial task prompt from the coordinator.
    pub initial_prompt: String,
}
