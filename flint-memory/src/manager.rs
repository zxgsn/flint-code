//! MemoryManager — orchestrator for the three-layer memory system.
//!
//! Ties together CoreMemory (Layer 1), MemoryStore (Layer 2), and
//! provides extraction and injection APIs for the agent loop.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::core::CoreMemory;
use crate::search::{self, SearchResult};
use crate::store::{self, MemoryStore};
use crate::types::{
    CoreBlock, ExtractedMemory, MemoryCategory, MemoryEntry, MemoryScope, TrustLevel,
};

/// Configuration for the memory manager.
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Maximum number of core memory blocks.
    pub max_core_blocks: usize,
    /// Character limit per core block.
    pub max_block_chars: usize,
    /// Whether to auto-extract facts after each turn.
    pub auto_extract: bool,
    /// Maximum number of memories to inject into context per turn.
    pub search_limit: usize,
    /// Similarity threshold for deduplication (Jaccard).
    pub dedup_threshold: f64,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_core_blocks: 8,
            max_block_chars: 2000,
            auto_extract: true,
            search_limit: 5,
            dedup_threshold: 0.7,
        }
    }
}

/// The main memory manager — entry point for all memory operations.
pub struct MemoryManager {
    core: CoreMemory,
    project_store: Option<MemoryStore>,
    global_store: MemoryStore,
    config: MemoryConfig,
    project_dir: Option<PathBuf>,
}

impl MemoryManager {
    /// Create a new MemoryManager. Loads stores from disk.
    pub fn new(config: MemoryConfig, project_dir: Option<&Path>) -> Result<Self> {
        let core_path = store::core_path()?;
        let core = CoreMemory::load_or_create(&core_path)?;

        let global_path = store::global_path()?;
        let global_store = MemoryStore::load_or_create(global_path, MemoryScope::Global)?;

        let project_store = if let Some(dir) = project_dir {
            let path = store::project_path(dir)?;
            Some(MemoryStore::load_or_create(path, MemoryScope::Project)?)
        } else {
            None
        };

        Ok(Self {
            core,
            project_store,
            global_store,
            config,
            project_dir: project_dir.map(|p| p.to_path_buf()),
        })
    }

    // ── Core Memory (Layer 1) ─────────────────────────────────────────────

    /// Get all core memory blocks.
    pub fn core_blocks(&self) -> &[CoreBlock] {
        self.core.blocks()
    }

    /// Update a core memory block.
    pub fn update_core(&mut self, label: &str, content: &str) -> Result<bool> {
        self.core.update(label, content)
    }

    /// Insert text into a core memory block.
    pub fn insert_core(&mut self, label: &str, content: &str) -> Result<bool> {
        self.core.insert(label, content)
    }

    /// Render core memory for the system prompt.
    pub fn render_core_for_prompt(&self) -> String {
        self.core.render_for_prompt()
    }

    // ── Archival Memory (Layer 2) ─────────────────────────────────────────

    /// Remember something — add a memory entry with dedup.
    /// If a similar entry already exists, reinforces it instead.
    /// Returns the entry ID.
    pub fn remember(
        &mut self,
        content: &str,
        category: MemoryCategory,
        tags: Vec<String>,
        scope: MemoryScope,
        trust: TrustLevel,
    ) -> Result<String> {
        let store = match scope {
            MemoryScope::Project => self.project_store.as_mut().context("no project context")?,
            MemoryScope::Global => &mut self.global_store,
        };

        // Dedup check: find similar existing entries
        let similar = store.find_similar(content, self.config.dedup_threshold);
        if let Some(existing_id) = similar.first() {
            // Reinforce existing memory
            if let Some(entry) = store.get_mut(existing_id) {
                entry.touch();
                entry.confidence = (entry.confidence + 0.1).min(1.0);
                store.save()?;
                return Ok(existing_id.clone());
            }
        }

        // No duplicate — add new entry
        let entry = MemoryEntry::new(category, content)
            .with_tags(tags)
            .with_scope(scope)
            .with_trust(trust);
        let id = store.add(entry);
        store.save()?;
        Ok(id)
    }

    /// Forget a memory (soft-delete).
    pub fn forget(&mut self, id: &str) -> Result<bool> {
        // Try project store first, then global
        if let Some(ref mut store) = self.project_store {
            if store.remove(id) {
                store.save()?;
                return Ok(true);
            }
        }
        if self.global_store.remove(id) {
            self.global_store.save()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Search archival memories across both scopes.
    pub fn search(
        &mut self,
        query: &str,
        scope: Option<MemoryScope>,
        limit: Option<usize>,
    ) -> Vec<SearchResult> {
        let limit = limit.unwrap_or(self.config.search_limit);
        let mut entries: Vec<&MemoryEntry> = Vec::new();

        match scope {
            Some(MemoryScope::Project) => {
                if let Some(ref store) = self.project_store {
                    entries.extend(store.list_active());
                }
            }
            Some(MemoryScope::Global) => {
                entries.extend(self.global_store.list_active());
            }
            None => {
                // Search both scopes
                if let Some(ref store) = self.project_store {
                    entries.extend(store.list_active());
                }
                entries.extend(self.global_store.list_active());
            }
        }

        let results = search::search(query, &entries, limit);

        // Touch accessed entries
        for result in &results {
            self.touch_entry(&result.entry.id);
        }

        results
    }

    /// List all memories in a scope.
    pub fn list(&self, scope: Option<MemoryScope>) -> Vec<&MemoryEntry> {
        let mut entries: Vec<&MemoryEntry> = Vec::new();
        match scope {
            Some(MemoryScope::Project) => {
                if let Some(ref store) = self.project_store {
                    entries.extend(store.list_active());
                }
            }
            Some(MemoryScope::Global) => {
                entries.extend(self.global_store.list_active());
            }
            None => {
                if let Some(ref store) = self.project_store {
                    entries.extend(store.list_active());
                }
                entries.extend(self.global_store.list_active());
            }
        }
        entries
    }

    /// Get memory counts per scope.
    pub fn counts(&self) -> (usize, usize, usize) {
        let core = self.core.blocks().len();
        let project = self
            .project_store
            .as_ref()
            .map(|s| s.active_count())
            .unwrap_or(0);
        let global = self.global_store.active_count();
        (core, project, global)
    }

    /// Format relevant memories for injection into the conversation.
    /// Returns None if no relevant memories found.
    pub fn format_relevant_memories(&mut self, query: &str) -> Option<String> {
        let results = self.search(query, None, Some(self.config.search_limit));
        if results.is_empty() {
            return None;
        }

        let mut output = String::from("[Relevant Memories]\n");
        for (i, result) in results.iter().enumerate() {
            let scope_label = match result.entry.scope {
                MemoryScope::Global => "global",
                MemoryScope::Project => "project",
            };
            output.push_str(&format!(
                "{}. [{}][{}] {}\n",
                i + 1,
                result.entry.category,
                scope_label,
                result.entry.content
            ));
        }
        Some(output)
    }

    // ── Extraction ────────────────────────────────────────────────────────

    /// Build a prompt for the LLM to extract memories from a conversation.
    pub fn extraction_prompt(&self, user_msg: &str, assistant_msg: &str) -> String {
        format!(
            r#"Analyze this conversation exchange and extract any facts, preferences, corrections, or patterns worth remembering long-term.

Rules:
- Only extract genuinely useful information (not trivial or ephemeral).
- Categories: "fact", "preference", "correction", "pattern"
- Each memory should be self-contained and clear.
- Include relevant tags for searchability.
- Output a JSON array. If nothing worth remembering, output an empty array: []

Conversation:
User: {}
Assistant: {}

Output format (JSON array):
[{{"category": "fact", "content": "...", "tags": ["tag1", "tag2"], "confidence": 0.8}}]"#,
            user_msg, assistant_msg
        )
    }

    /// Parse extracted memories from LLM output.
    pub fn parse_extracted(&self, json_str: &str) -> Vec<ExtractedMemory> {
        // Try to find JSON array in the output
        let json_str = json_str.trim();
        let json_str = if let Some(start) = json_str.find('[') {
            if let Some(end) = json_str.rfind(']') {
                &json_str[start..=end]
            } else {
                json_str
            }
        } else {
            json_str
        };

        serde_json::from_str(json_str).unwrap_or_default()
    }

    /// Store extracted memories.
    pub fn store_extracted(
        &mut self,
        memories: &[ExtractedMemory],
        scope: MemoryScope,
    ) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for extracted in memories {
            let category = MemoryCategory::from_str_loose(&extracted.category);
            let trust = if extracted.confidence >= 0.8 {
                TrustLevel::High
            } else if extracted.confidence >= 0.5 {
                TrustLevel::Medium
            } else {
                TrustLevel::Low
            };
            let id = self.remember(
                &extracted.content,
                category,
                extracted.tags.clone(),
                scope,
                trust,
            )?;
            ids.push(id);
        }
        Ok(ids)
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    /// Touch an entry by ID in whichever store it lives.
    fn touch_entry(&mut self, id: &str) {
        if let Some(ref mut store) = self.project_store {
            if let Some(entry) = store.get_mut(id) {
                entry.touch();
                let _ = store.save();
                return;
            }
        }
        if let Some(entry) = self.global_store.get_mut(id) {
            entry.touch();
            let _ = self.global_store.save();
        }
    }
}
