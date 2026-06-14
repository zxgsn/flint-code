//! Agent runtime for flint — loop, tools, and session management.

pub mod agent;
pub mod checkpoint;
pub mod session;
pub mod shell;
pub mod todo;
pub mod tool;

pub use agent::{run_turn, TurnStats};
pub use checkpoint::CheckpointStore;
pub use session::{Session, SessionMeta};
pub use todo::{TodoStore, is_confirmation, format_todo_list, all_done, weighted_completion_confidence};
pub use tool::{Tool, ToolContext, ToolRegistry};

/// Structured event emitted during `run_turn` for real-time observation.
/// Callers can use this to display, log, or forward agent activity.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// LLM is thinking (request sent, waiting for response).
    Thinking,
    /// Text delta from the LLM stream.
    TextDelta(String),
    /// LLM requested a tool call.
    ToolCallStart { name: String, input_preview: String },
    /// A tool call completed.
    ToolCallEnd { name: String, success: bool, preview: String, elapsed_ms: u64 },
    /// The turn completed with final text.
    TurnComplete { text: String, llm_calls: u32, tool_calls: u32, chars: usize, elapsed_ms: u64 },
}

/// Callback for receiving real-time agent events.
/// Return `true` to also print to terminal (default behavior), `false` to suppress printing.
pub type EventCallback = Box<dyn Fn(&AgentEvent) -> bool + Send + Sync>;
