//! Core types for the flint memory system.
//!
//! Three-layer architecture inspired by Letta/MemGPT:
//! - **Core Memory**: Always-in-context blocks (persona, preferences, project facts)
//! - **Archival Memory**: Long-term searchable knowledge store
//! - **Recall Memory**: Session-local extracted facts (in-memory only)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Core Memory Blocks (Layer 1 — always in system prompt) ────────────────

/// A core memory block — always visible to the LLM in the system prompt.
/// Inspired by Letta's Block concept.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreBlock {
    /// Label identifying this block's role (e.g. "persona", "human", "project").
    pub label: String,
    /// The text content of this block.
    pub content: String,
    /// Character limit for this block.
    #[serde(default = "default_block_limit")]
    pub limit: usize,
    /// Whether this block is read-only (cannot be edited by the agent).
    #[serde(default)]
    pub read_only: bool,
    /// When this block was last updated.
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

fn default_block_limit() -> usize {
    2000
}

impl CoreBlock {
    pub fn new(label: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            content: content.into(),
            limit: default_block_limit(),
            read_only: false,
            updated_at: Utc::now(),
        }
    }

    /// Update the content, respecting the character limit.
    /// Returns true if the update was applied.
    pub fn update(&mut self, new_content: &str) -> bool {
        if self.read_only {
            return false;
        }
        if new_content.len() > self.limit {
            return false;
        }
        self.content = new_content.to_string();
        self.updated_at = Utc::now();
        true
    }

    /// Render this block for inclusion in the system prompt.
    pub fn render(&self) -> String {
        format!("[{}]\n{}", self.label, self.content)
    }
}

// ── Archival Memory (Layer 2 — long-term searchable) ──────────────────────

/// Category of an archival memory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    /// A factual observation (default).
    Fact,
    /// A user preference or working style.
    Preference,
    /// A correction to a previous understanding.
    Correction,
    /// A learned pattern or best practice.
    Pattern,
    /// User-defined category.
    Custom(String),
}

impl Default for MemoryCategory {
    fn default() -> Self {
        Self::Fact
    }
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fact => write!(f, "fact"),
            Self::Preference => write!(f, "preference"),
            Self::Correction => write!(f, "correction"),
            Self::Pattern => write!(f, "pattern"),
            Self::Custom(s) => write!(f, "{}", s),
        }
    }
}

impl MemoryCategory {
    /// Parse from a string (for LLM extraction output).
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "fact" | "observation" | "knowledge" => Self::Fact,
            "preference" | "user_preference" => Self::Preference,
            "correction" | "fix" => Self::Correction,
            "pattern" | "best_practice" | "lesson" => Self::Pattern,
            other => Self::Custom(other.to_string()),
        }
    }

    /// Category bonus for scoring (higher = more important).
    pub fn score_bonus(&self) -> f64 {
        match self {
            Self::Correction => 50.0,
            Self::Preference => 30.0,
            Self::Pattern => 25.0,
            Self::Fact => 20.0,
            Self::Custom(_) => 5.0,
        }
    }

    /// Confidence half-life in days (how fast confidence decays).
    pub fn half_life_days(&self) -> f64 {
        match self {
            Self::Correction => 365.0,
            Self::Preference => 180.0,
            Self::Pattern => 90.0,
            Self::Fact => 60.0,
            Self::Custom(_) => 45.0,
        }
    }
}

/// Trust level indicating the source of the memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// User explicitly stated this.
    High,
    /// Observed from conversation patterns (default).
    Medium,
    /// Inferred by the LLM.
    Low,
}

impl Default for TrustLevel {
    fn default() -> Self {
        Self::Medium
    }
}

impl TrustLevel {
    pub fn score_multiplier(&self) -> f64 {
        match self {
            Self::High => 1.5,
            Self::Medium => 1.0,
            Self::Low => 0.7,
        }
    }
}

/// Scope of a memory — global (user-level) or project-local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Global,
    Project,
}

impl Default for MemoryScope {
    fn default() -> Self {
        Self::Project
    }
}

impl std::fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Global => write!(f, "global"),
            Self::Project => write!(f, "project"),
        }
    }
}

/// A single archival memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unique ID (format: "mem_{uuid}").
    pub id: String,
    /// Category of this memory.
    pub category: MemoryCategory,
    /// The content/text of the memory.
    pub content: String,
    /// Searchable tags for keyword matching.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Scope: global or project-local.
    #[serde(default)]
    pub scope: MemoryScope,
    /// Trust level of this memory.
    #[serde(default)]
    pub trust: TrustLevel,
    /// How many times this memory has been accessed.
    #[serde(default)]
    pub access_count: u32,
    /// When this memory was created.
    pub created_at: DateTime<Utc>,
    /// When this memory was last updated or accessed.
    pub updated_at: DateTime<Utc>,
    /// Whether this memory is active (false if superseded).
    #[serde(default = "default_true")]
    pub active: bool,
    /// ID of the memory that superseded this one.
    #[serde(default)]
    pub superseded_by: Option<String>,
    /// Confidence score (0.0-1.0), decays over time, boosted by use.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

fn default_true() -> bool {
    true
}

fn default_confidence() -> f32 {
    0.8
}

impl MemoryEntry {
    /// Create a new memory entry with auto-generated ID.
    pub fn new(category: MemoryCategory, content: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: format!("mem_{}", uuid::Uuid::new_v4()),
            category,
            content: content.into(),
            tags: Vec::new(),
            scope: MemoryScope::default(),
            trust: TrustLevel::default(),
            access_count: 0,
            created_at: now,
            updated_at: now,
            active: true,
            superseded_by: None,
            confidence: default_confidence(),
        }
    }

    /// Builder: set tags.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Builder: set scope.
    pub fn with_scope(mut self, scope: MemoryScope) -> Self {
        self.scope = scope;
        self
    }

    /// Builder: set trust level.
    pub fn with_trust(mut self, trust: TrustLevel) -> Self {
        self.trust = trust;
        self
    }

    /// Compute the effective confidence, accounting for time decay and access boost.
    pub fn effective_confidence(&self) -> f32 {
        let age_hours = (Utc::now() - self.updated_at).num_hours().max(0) as f32;
        let half_life_hours = self.category.half_life_days() as f32 * 24.0;
        let decay = 2.0_f32.powf(-age_hours / half_life_hours);
        let access_boost = (self.access_count as f32 + 1.0).ln() * 0.1;
        (self.confidence * decay + access_boost).min(1.0)
    }

    /// Record an access (increments count, refreshes updated_at).
    pub fn touch(&mut self) {
        self.access_count += 1;
        self.updated_at = Utc::now();
    }

    /// Mark this memory as superseded by another.
    pub fn supersede(&mut self, new_id: &str) {
        self.active = false;
        self.superseded_by = Some(new_id.to_string());
    }

    /// Normalize content for keyword search.
    pub fn search_text(&self) -> String {
        let mut text = self.content.to_lowercase();
        for tag in &self.tags {
            text.push(' ');
            text.push_str(&tag.to_lowercase());
        }
        text
    }
}

// ── Extraction result from LLM ────────────────────────────────────────────

/// A memory extracted from conversation by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMemory {
    /// Category of the extracted memory.
    pub category: String,
    /// The content/fact to remember.
    pub content: String,
    /// Optional tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Confidence of the extraction (0.0-1.0).
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

// ── Recall Memory (Layer 3 — session-local) ───────────────────────────────

/// A fact extracted during the current session, held in memory only.
#[derive(Debug, Clone)]
pub struct RecallEntry {
    /// The extracted content.
    pub content: String,
    /// Source: which message index triggered this extraction.
    pub source_index: usize,
    /// When it was extracted.
    pub extracted_at: DateTime<Utc>,
}

// ── Memory Store Metadata ─────────────────────────────────────────────────

/// Metadata for a persisted memory store file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreMeta {
    /// Schema version for migration.
    pub version: u32,
    /// Scope of this store.
    pub scope: MemoryScope,
    /// When the store was created.
    pub created_at: DateTime<Utc>,
    /// When the store was last modified.
    pub updated_at: DateTime<Utc>,
    /// Number of entries.
    pub entry_count: usize,
}
