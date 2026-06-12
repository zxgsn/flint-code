//! In-session todo list for tracking task progress.
//!
//! The LLM creates and manages todos via the `todo` tool.
//! The auto-poke system reads incomplete todos to decide whether to
//! send a follow-up message after a turn completes.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Status of a todo item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    pub fn is_incomplete(&self) -> bool {
        !matches!(self, TodoStatus::Completed | TodoStatus::Cancelled)
    }
}

/// A single todo item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: u32,
    pub title: String,
    pub status: TodoStatus,
}

/// Thread-safe todo store shared between the TodoTool and the REPL.
pub type TodoStore = Arc<Mutex<Vec<TodoItem>>>;

/// Create a new empty todo store.
pub fn new_store() -> TodoStore {
    Arc::new(Mutex::new(Vec::new()))
}

/// Count incomplete todos in the store.
pub fn incomplete_count(store: &TodoStore) -> usize {
    store.lock().unwrap().iter().filter(|t| t.status.is_incomplete()).count()
}

/// Get all incomplete todos.
pub fn incomplete_todos(store: &TodoStore) -> Vec<TodoItem> {
    store.lock().unwrap().iter().filter(|t| t.status.is_incomplete()).cloned().collect()
}
