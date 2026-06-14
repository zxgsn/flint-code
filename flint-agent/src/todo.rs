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

/// Priority level for weighted confidence calculation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

impl TodoPriority {
    /// Weight for weighted confidence calculation (jcode convention).
    pub fn weight(&self) -> u8 {
        match self {
            TodoPriority::High => 3,
            TodoPriority::Medium => 2,
            TodoPriority::Low => 1,
        }
    }
}

impl Default for TodoPriority {
    fn default() -> Self {
        TodoPriority::Medium
    }
}

/// A single todo item.
///
/// Optional fields use `#[serde(default)]` for backward compatibility
/// with sessions saved before the field was added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: u32,
    pub title: String,
    pub status: TodoStatus,
    /// Priority level for weighted confidence calculation.
    #[serde(default)]
    pub priority: TodoPriority,
    /// Forward-looking confidence that this todo can be completed correctly (0-100).
    /// Lower values indicate higher complexity — candidates for swarm delegation.
    #[serde(default)]
    pub confidence: Option<u8>,
    /// Confidence recorded when the todo is marked completed (0-100).
    /// Used by auto-poke to compute overall completion quality.
    #[serde(default)]
    pub completion_confidence: Option<u8>,
    /// IDs of todos that must be completed before this one can start.
    #[serde(default)]
    pub blocked_by: Vec<u32>,
    /// ID of the swarm agent assigned to this todo (if any).
    #[serde(default)]
    pub assigned_to: Option<String>,
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

/// Get incomplete todos that have no dependencies and are not assigned to an agent.
/// These are candidates for parallel execution via swarm.
pub fn parallelizable_todos(store: &TodoStore) -> Vec<TodoItem> {
    store.lock().unwrap().iter()
        .filter(|t| t.status.is_incomplete())
        .filter(|t| t.blocked_by.is_empty() && t.assigned_to.is_none())
        .cloned()
        .collect()
}

/// Compute weighted confidence across all completed todos.
///
/// Weights: high=3, medium=2, low=1 (jcode convention).
/// Returns (weighted_average, total_todos, completed_count, below_threshold_count).
pub fn weighted_completion_confidence(store: &TodoStore) -> ConfidenceSummary {
    let todos = store.lock().unwrap();
    let completed: Vec<_> = todos.iter()
        .filter(|t| t.status == TodoStatus::Completed)
        .collect();

    if completed.is_empty() {
        return ConfidenceSummary {
            weighted_avg: 0,
            total: todos.len(),
            completed: 0,
            cancelled: todos.iter().filter(|t| t.status == TodoStatus::Cancelled).count(),
            below_threshold: 0,
            missing_confidence: todos.len(),
            lowest_confidence: None,
        };
    }

    let threshold = 90u8;
    let mut weighted_sum: f64 = 0.0;
    let mut total_weight: f64 = 0.0;
    let mut below_threshold = 0usize;
    let mut missing_confidence = 0usize;
    let mut lowest: Option<u8> = None;

    for t in &completed {
        let weight = t.priority.weight() as f64;
        match t.completion_confidence {
            Some(c) => {
                weighted_sum += c as f64 * weight;
                total_weight += weight;
                if c < threshold {
                    below_threshold += 1;
                }
                lowest = Some(lowest.map_or(c, |l| l.min(c)));
            }
            None => {
                missing_confidence += 1;
                // Assume 50% for missing confidence (conservative)
                weighted_sum += 50.0 * weight;
                total_weight += weight;
                below_threshold += 1;
            }
        }
    }

    let weighted_avg = if total_weight > 0.0 {
        (weighted_sum / total_weight) as u8
    } else {
        0
    };

    ConfidenceSummary {
        weighted_avg,
        total: todos.len(),
        completed: completed.len(),
        cancelled: todos.iter().filter(|t| t.status == TodoStatus::Cancelled).count(),
        below_threshold,
        missing_confidence,
        lowest_confidence: lowest,
    }
}

/// Result of weighted confidence calculation.
#[derive(Debug)]
pub struct ConfidenceSummary {
    /// Weighted average confidence (0-100).
    pub weighted_avg: u8,
    /// Total number of todos.
    pub total: usize,
    /// Number of completed todos.
    pub completed: usize,
    /// Number of cancelled todos.
    pub cancelled: usize,
    /// Number of completed todos below the 90% threshold.
    pub below_threshold: usize,
    /// Number of completed todos missing completion_confidence.
    pub missing_confidence: usize,
    /// Lowest completion confidence among completed todos.
    pub lowest_confidence: Option<u8>,
}

/// Confidence threshold for validation (jcode convention).
pub const CONFIDENCE_THRESHOLD: u8 = 90;

/// Check if the user's input is a confirmation to start executing todos.
/// Supports both Chinese and English confirmation phrases.
pub fn is_confirmation(input: &str) -> bool {
    let lower = input.trim().to_lowercase();
    let confirmations = [
        "直接执行", "开始执行", "执行吧", "开始吧", "确认", "可以", "没问题",
        "execute", "start", "go", "confirm", "yes", "proceed", "do it",
        "begin", "run", "ok", "okay",
    ];
    confirmations.iter().any(|c| lower.contains(c))
}

/// Generate a summary of all todos for display to the user.
pub fn format_todo_list(store: &TodoStore) -> String {
    let todos = store.lock().unwrap();
    if todos.is_empty() {
        return "No todos.".to_string();
    }

    let mut lines = Vec::new();
    lines.push("## 📋 任务清单".to_string());
    lines.push("".to_string());

    // Group by status
    let pending: Vec<_> = todos.iter().filter(|t| t.status == TodoStatus::Pending).collect();
    let in_progress: Vec<_> = todos.iter().filter(|t| t.status == TodoStatus::InProgress).collect();
    let completed: Vec<_> = todos.iter().filter(|t| t.status == TodoStatus::Completed).collect();
    let cancelled: Vec<_> = todos.iter().filter(|t| t.status == TodoStatus::Cancelled).collect();

    let format_item = |t: &TodoItem| -> String {
        let checkbox = match t.status {
            TodoStatus::Pending => "⬜",
            TodoStatus::InProgress => "🔄",
            TodoStatus::Completed => "✅",
            TodoStatus::Cancelled => "❌",
        };

        let priority = match t.priority {
            TodoPriority::High => " ",
            TodoPriority::Medium => " ",
            TodoPriority::Low => " ",
        };

        let mut extra = Vec::new();
        if let Some(c) = t.confidence {
            extra.push(format!("{}%", c));
        }
        if !t.blocked_by.is_empty() {
            let deps: Vec<String> = t.blocked_by.iter().map(|id| format!("#{}", id)).collect();
            extra.push(format!("依赖{}", deps.join(",")));
        }
        if let Some(ref agent) = t.assigned_to {
            extra.push(format!("@{}", agent));
        }

        let extra_str = if extra.is_empty() {
            String::new()
        } else {
            format!(" `{}`", extra.join(" "))
        };

        format!("{} {} {}{}{}", checkbox, priority, t.title, extra_str, if t.status == TodoStatus::InProgress { " ⏳" } else { "" })
    };

    if !in_progress.is_empty() {
        lines.push("### 🔄 进行中".to_string());
        for t in &in_progress {
            lines.push(format_item(t));
        }
        lines.push("".to_string());
    }

    if !pending.is_empty() {
        lines.push("### ⏳ 待办".to_string());
        for t in &pending {
            lines.push(format_item(t));
        }
        lines.push("".to_string());
    }

    if !completed.is_empty() {
        lines.push("### ✅ 已完成".to_string());
        for t in &completed {
            lines.push(format_item(t));
        }
        lines.push("".to_string());
    }

    if !cancelled.is_empty() {
        lines.push("### ❌ 已取消".to_string());
        for t in &cancelled {
            lines.push(format_item(t));
        }
        lines.push("".to_string());
    }

    // Summary
    let total = todos.len();
    let done = completed.len() + cancelled.len();
    let remaining = total - done;
    let progress = if total > 0 { (done as f64 / total as f64 * 100.0) as u8 } else { 0 };

    lines.push("---".to_string());
    lines.push(format!("📊 进度: {}/{} ({}%) | 待办: {}", done, total, progress, remaining));

    lines.join("\n")
}

/// Check if all todos are completed or cancelled.
pub fn all_done(store: &TodoStore) -> bool {
    store.lock().unwrap().iter().all(|t| !t.status.is_incomplete())
}
