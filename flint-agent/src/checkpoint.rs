//! Per-turn file snapshots for rollback support.
//!
//! Before `write` or `edit` tools modify a file, the original content
//! is saved into a `TurnCheckpoint`. The REPL accumulates checkpoints
//! across turns and `/undo` restores the most recent one.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A snapshot of a single file before modification.
#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub path: PathBuf,
    /// Original content before the tool modified it.
    /// `None` means the file didn't exist (it was newly created).
    pub original_content: Option<String>,
}

/// All file snapshots for one turn.
#[derive(Debug, Clone)]
pub struct TurnCheckpoint {
    pub turn_number: u32,
    pub snapshots: Vec<FileSnapshot>,
}

/// Thread-safe checkpoint store shared between tools and the REPL.
pub type CheckpointStore = Arc<Mutex<Vec<TurnCheckpoint>>>;

/// Create a new empty checkpoint store.
pub fn new_store() -> CheckpointStore {
    Arc::new(Mutex::new(Vec::new()))
}

/// Record a file snapshot before a tool modifies it.
/// Deduplicates: if the same file was already snapshotted this turn, skip.
pub fn record_snapshot(store: &CheckpointStore, turn: u32, path: PathBuf, original: Option<String>) {
    let mut checkpoints = store.lock().unwrap();
    let cp = checkpoints.iter_mut().find(|c| c.turn_number == turn);
    let snapshot = FileSnapshot { path, original_content: original };

    match cp {
        Some(existing) => {
            // Don't duplicate if already snapshotted this turn
            if !existing.snapshots.iter().any(|s| s.path == snapshot.path) {
                existing.snapshots.push(snapshot);
            }
        }
        None => {
            checkpoints.push(TurnCheckpoint {
                turn_number: turn,
                snapshots: vec![snapshot],
            });
        }
    }
}

/// Pop the most recent checkpoint (for `/undo`).
/// Returns None if no checkpoints exist.
pub fn pop_latest(store: &CheckpointStore) -> Option<TurnCheckpoint> {
    store.lock().unwrap().pop()
}

/// Number of available checkpoints.
pub fn checkpoint_count(store: &CheckpointStore) -> usize {
    store.lock().unwrap().len()
}
