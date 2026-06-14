//! Auto-poking: automatically send a follow-up message when incomplete todos remain.
//!
//! After each turn, if the todo store has incomplete items, the system injects
//! a "continue working" message and runs another turn. This drives the agent
//! to finish multi-step tasks without manual prompting.
//!
//! ## Safety (infinite loop prevention)
//!
//! - Max consecutive pokes (default: 10). Stops even if todos remain incomplete.
//! - Automatic stop when all todos are completed or cancelled.
//! - User can disable at any time via `/poke off`.
//! - Non-retryable errors (auth, network, billing) stop auto-poke immediately.

use flint_agent::todo::TodoStore;

/// Max consecutive pokes before auto-poke gives up.
const DEFAULT_MAX_POKES: u32 = 10;

/// Auto-poke state, owned by the REPL loop.
pub struct AutoPoke {
    /// Whether auto-poke is currently enabled.
    pub enabled: bool,
    /// Consecutive poke count since last user message. Resets on user input.
    pub consecutive_pokes: u32,
    /// Max consecutive pokes before stopping.
    pub max_pokes: u32,
    /// The todo store shared with the TodoTool.
    pub store: TodoStore,
    /// Whether swarm is enabled — controls parallel suggestions.
    pub swarm_enabled: bool,
}

impl AutoPoke {
    pub fn new(store: TodoStore) -> Self {
        Self {
            enabled: true,
            consecutive_pokes: 0,
            max_pokes: DEFAULT_MAX_POKES,
            store,
            swarm_enabled: false,
        }
    }

    /// Reset the consecutive poke counter (called when user sends a message).
    pub fn reset_counter(&mut self) {
        self.consecutive_pokes = 0;
    }

    /// Check if auto-poke should fire after a turn completes.
    /// Returns the poke message if it should fire, None otherwise.
    pub fn should_poke(&mut self) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let todos = flint_agent::todo::incomplete_todos(&self.store);

        // All todos done — check confidence and disarm
        if todos.is_empty() {
            self.enabled = false;
            return self.build_confidence_summary();
        }

        if self.consecutive_pokes >= self.max_pokes {
            // Safety limit reached
            eprintln!(
                "\x1b[33m  [auto-poke] stopped: {} consecutive pokes reached (limit: {})\x1b[0m",
                self.consecutive_pokes, self.max_pokes
            );
            self.enabled = false;
            return None;
        }

        self.consecutive_pokes += 1;
        let incomplete = todos.len();

        // Check for parallelizable todos (no dependencies, not assigned)
        let parallelizable = flint_agent::todo::parallelizable_todos(&self.store);
        let can_parallelize = self.swarm_enabled && parallelizable.len() >= 2;

        // Format todo list
        let todo_list: String = todos
            .iter()
            .take(5)
            .map(|t| {
                let mut info = format!("  #{} [{}] {}", t.id, format!("{:?}", t.status).to_lowercase(), t.title);
                if let Some(c) = t.confidence {
                    info.push_str(&format!(" (conf={})", c));
                }
                if !t.blocked_by.is_empty() {
                    info.push_str(&format!(" (blocked by {:?})", t.blocked_by));
                }
                info
            })
            .collect::<Vec<_>>()
            .join("\n");
        let more = if incomplete > 5 {
            format!("\n  ... and {} more", incomplete - 5)
        } else {
            String::new()
        };

        // Build poke message with optional parallel suggestion
        if can_parallelize {
            let parallel_list: String = parallelizable
                .iter()
                .take(5)
                .map(|t| format!("  #{} {}", t.id, t.title))
                .collect::<Vec<_>>()
                .join("\n");
            Some(format!(
                "You have {} incomplete todo{}. {} can run in parallel:\n{}\n\n\
                 Consider using `swarm spawn` for independent tasks, \
                 or continue sequentially:\n{}{}\n\n\
                 Use the todo tool to update status as you complete each item.",
                incomplete,
                if incomplete == 1 { "" } else { "s" },
                parallelizable.len(),
                parallel_list,
                todo_list,
                more,
            ))
        } else {
            Some(format!(
                "You have {} incomplete todo{}. Continue working:\n{}{}\n\n\
                 Use the todo tool to update status as you complete each item.",
                incomplete,
                if incomplete == 1 { "" } else { "s" },
                todo_list,
                more,
            ))
        }
    }

    /// Build a confidence summary message when all todos are complete.
    /// Returns None if confidence is sufficient or no todos exist.
    fn build_confidence_summary(&self) -> Option<String> {
        let summary = flint_agent::todo::weighted_completion_confidence(&self.store);

        if summary.completed == 0 {
            return None;
        }

        eprintln!(
            "\x1b[90m  [auto-poke] all done: {}/{} completed, weighted confidence: {}%\x1b[0m",
            summary.completed, summary.total, summary.weighted_avg
        );

        if summary.weighted_avg >= flint_agent::todo::CONFIDENCE_THRESHOLD {
            // Confidence is sufficient — stop cleanly
            eprintln!("\x1b[32m  [auto-poke] confidence above threshold ({}%)\x1b[0m",
                flint_agent::todo::CONFIDENCE_THRESHOLD);
            return None;
        }

        // Confidence below threshold — send validation prompt
        let mut msg = format!(
            "All {} todos completed. Weighted confidence: {}% (threshold: {}%).",
            summary.completed,
            summary.weighted_avg,
            flint_agent::todo::CONFIDENCE_THRESHOLD
        );

        if summary.below_threshold > 0 {
            msg.push_str(&format!(
                "\n{} todo(s) below threshold.",
                summary.below_threshold
            ));
        }

        if summary.missing_confidence > 0 {
            msg.push_str(&format!(
                "\n{} todo(s) missing completion_confidence.",
                summary.missing_confidence
            ));
        }

        if let Some(lowest) = summary.lowest_confidence {
            msg.push_str(&format!("\nLowest confidence: {}%.", lowest));
        }

        msg.push_str(
            "\n\nConsider validating before finalizing. If you are confident \
             everything is correct, you may stop. Otherwise, review the work \
             and run additional tests."
        );

        Some(msg)
    }

    /// Check if an error message indicates a non-retryable failure.
    /// Returns true if auto-poke should be stopped.
    pub fn is_non_retryable_error(error_msg: &str) -> bool {
        let lower = error_msg.to_lowercase();
        let markers = [
            "401", "403", "402",
            "context_length_exceeded",
            "model_not_found",
            "billing", "credits", "quota",
            "invalid_api_key",
            "authentication",
            "dns error", "network unreachable",
            "connection refused",
        ];
        markers.iter().any(|m| lower.contains(m))
    }

    /// Stop auto-poke due to a non-retryable error.
    pub fn stop_for_error(&mut self, error_msg: &str) {
        if Self::is_non_retryable_error(error_msg) {
            self.enabled = false;
            // Safe truncation at char boundary
            let display = if error_msg.len() > 80 {
                let end = error_msg.char_indices()
                    .nth(80)
                    .map(|(i, _)| i)
                    .unwrap_or(error_msg.len());
                &error_msg[..end]
            } else {
                error_msg
            };
            eprintln!(
                "\x1b[33m  [auto-poke] stopped due to error: {}\x1b[0m",
                display
            );
        }
    }
}
