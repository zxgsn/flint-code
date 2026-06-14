//! Swarm coordination system for flint.
//!
//! In-process tokio task architecture. Sub-agents run as tokio tasks
//! sharing the same Provider (via Arc). Each has its own Session and
//! ToolRegistry. Communication is via oneshot channels (results) and
//! mpsc channels (real-time output events).
//!
//! ## Architecture
//!
//! ```text
//! Main Agent (Coordinator)
//!     │ swarm tool: spawn/result/status/stop
//!     ▼
//! SwarmManager (in-process)
//!     ├─ Agent 1 (tokio task, own Session, shared Provider)
//!     ├─ Agent 2 (tokio task, own Session, shared Provider)
//!     └─ display_loop (real-time [agent_id] prefixed output)
//! ```

pub mod agent;
pub mod endpoint;
pub mod log;
pub mod manager;
pub mod output;
pub mod router;
pub mod tool;
pub mod types;

pub use agent::{InputRequest, InputResponse};
pub use manager::SwarmManager;
pub use router::{AgentResult, MessageRouter};
pub use tool::{ProviderFactory, register_swarm_tools};
pub use types::{AgentNotification, AgentStatus, FileAccessNotification, SpawnContext, SwarmConfig, TaskItem, TaskStatus};
