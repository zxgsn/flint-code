//! Message types for inter-agent communication.

use crate::types::AgentStatus;

/// Request from coordinator to a sub-agent.
#[derive(Debug, Clone)]
pub enum AgentRequest {
    /// Execute a task with the given prompt.
    ExecuteTask { task_id: String, prompt: String },
    /// Stop the agent.
    Stop,
}

/// Response from a sub-agent to the coordinator.
#[derive(Debug, Clone)]
pub enum AgentResponse {
    /// Task completed successfully.
    TaskComplete {
        task_id: String,
        agent_id: String,
        result: String,
    },
    /// Task failed with an error.
    TaskFailed {
        task_id: String,
        agent_id: String,
        error: String,
    },
    /// Agent status changed.
    StatusUpdate {
        agent_id: String,
        status: AgentStatus,
    },
}
