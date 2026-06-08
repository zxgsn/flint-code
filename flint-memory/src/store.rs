//! File-based JSON persistence for memory stores.
//!
//! Storage layout:
//! - `~/.flint/memory/core.json` — CoreMemory blocks
//! - `~/.flint/memory/global.json` — global-scoped archival memories
//! - `~/.flint/memory/projects/{hash}.json` — project-scoped archival memories

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::types::{MemoryEntry, MemoryScope, StoreMeta};

const STORE_VERSION: u32 = 1;

/// The on-disk format for a memory store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStoreFile {
    pub meta: StoreMeta,
    pub entries: Vec<MemoryEntry>,
}

/// In-memory memory store with persistence support.
pub struct MemoryStore {
    pub entries: Vec<MemoryEntry>,
    path: PathBuf,
    scope: MemoryScope,
}

impl MemoryStore {
    /// Load or create a store at the given path.
    pub fn load_or_create(path: PathBuf, scope: MemoryScope) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let file: MemoryStoreFile = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            Ok(Self {
                entries: file.entries,
                path,
                scope,
            })
        } else {
            // Create parent directories
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(Self {
                entries: Vec::new(),
                path,
                scope,
            })
        }
    }

    /// Save the store to disk.
    pub fn save(&self) -> Result<()> {
        let meta = StoreMeta {
            version: STORE_VERSION,
            scope: self.scope,
            created_at: self.entries.first().map(|e| e.created_at).unwrap_or_else(Utc::now),
            updated_at: Utc::now(),
            entry_count: self.entries.len(),
        };
        let file = MemoryStoreFile {
            meta,
            entries: self.entries.clone(),
        };
        let json = serde_json::to_string_pretty(&file)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    /// Add an entry. Returns the entry's ID.
    pub fn add(&mut self, mut entry: MemoryEntry) -> String {
        entry.scope = self.scope;
        let id = entry.id.clone();
        self.entries.push(entry);
        id
    }

    /// Get an entry by ID.
    pub fn get(&self, id: &str) -> Option<&MemoryEntry> {
        self.entries.iter().find(|e| e.id == id && e.active)
    }

    /// Get a mutable entry by ID.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut MemoryEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    /// Remove an entry by ID (soft-delete: marks inactive).
    pub fn remove(&mut self, id: &str) -> bool {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.active = false;
            true
        } else {
            false
        }
    }

    /// Hard-remove an entry by ID.
    pub fn hard_remove(&mut self, id: &str) -> Option<MemoryEntry> {
        if let Some(pos) = self.entries.iter().position(|e| e.id == id) {
            Some(self.entries.remove(pos))
        } else {
            None
        }
    }

    /// List all active entries, sorted by updated_at descending.
    pub fn list_active(&self) -> Vec<&MemoryEntry> {
        let mut entries: Vec<&MemoryEntry> =
            self.entries.iter().filter(|e| e.active).collect();
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        entries
    }

    /// Count active entries.
    pub fn active_count(&self) -> usize {
        self.entries.iter().filter(|e| e.active).count()
    }

    /// Find entries with duplicate or very similar content.
    /// Returns IDs of existing entries that are similar to the given content.
    pub fn find_similar(&self, content: &str, threshold: f64) -> Vec<String> {
        let content_lower = content.to_lowercase();
        let content_words: Vec<&str> = content_lower.split_whitespace().collect();

        if content_words.is_empty() {
            return Vec::new();
        }

        self.entries
            .iter()
            .filter(|e| e.active)
            .filter_map(|e| {
                let lowered = e.content.to_lowercase();
                let entry_words: Vec<&str> = lowered.split_whitespace().collect();
                let similarity = jaccard_similarity(&content_words, &entry_words);
                if similarity >= threshold {
                    Some(e.id.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

/// Compute Jaccard similarity between two word sets.
fn jaccard_similarity(a: &[&str], b: &[&str]) -> f64 {
    use std::collections::HashSet;
    let set_a: HashSet<&str> = a.iter().copied().collect();
    let set_b: HashSet<&str> = b.iter().copied().collect();
    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────

/// Get the base memory directory: `~/.flint/memory/`
pub fn memory_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".flint").join("memory"))
}

/// Get the core memory file path: `~/.flint/memory/core.json`
pub fn core_path() -> Result<PathBuf> {
    Ok(memory_dir()?.join("core.json"))
}

/// Get the global memory store path: `~/.flint/memory/global.json`
pub fn global_path() -> Result<PathBuf> {
    Ok(memory_dir()?.join("global.json"))
}

/// Hash a path to a 16-char hex string for project-scoped storage.
pub fn hash_project_dir(project_dir: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    project_dir.to_string_lossy().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Get the project memory store path: `~/.flint/memory/projects/{hash}.json`
pub fn project_path(project_dir: &Path) -> Result<PathBuf> {
    let hash = hash_project_dir(project_dir);
    Ok(memory_dir()?.join("projects").join(format!("{}.json", hash)))
}
