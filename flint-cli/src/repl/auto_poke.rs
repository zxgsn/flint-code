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
}

impl AutoPoke {
    pub fn new(store: TodoStore) -> Self {
        Self {
            enabled: true,
            consecutive_pokes: 0,
            max_pokes: DEFAULT_MAX_POKES,
            store,
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

        let incomplete = flint_agent::todo::incomplete_count(&self.store);
        if incomplete == 0 {
            // All done — no poke needed
            return None;
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
        Some(format!(
            "You have {} incomplete todo{}. Continue working, or use the todo tool to update status.",
            incomplete,
            if incomplete == 1 { "" } else { "s" },
        ))
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
            eprintln!(
                "\x1b[33m  [auto-poke] stopped due to error: {}\x1b[0m",
                if error_msg.len() > 80 { &error_msg[..80] } else { error_msg }
            );
        }
    }
}
