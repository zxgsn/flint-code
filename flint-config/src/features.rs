//! Feature toggles — all enabled by default.

use serde::{Deserialize, Serialize};

/// Individual feature identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Skills,
    Memory,
    Compaction,
    Permissions,
}

// ── Per-feature config blocks ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillConfig {
    pub enabled: bool,
    /// Skill directories to load from. Later dirs override earlier ones.
    /// Default: `~/.flint/skills` and `.flint/skills` (project-level).
    #[serde(default)]
    pub directories: Vec<std::path::PathBuf>,
    /// Names of skills to activate. Empty = activate all loaded skills.
    #[serde(default)]
    pub active: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionConfig {
    pub enabled: bool,
}

// ── Aggregate feature container ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Features {
    pub skills: SkillConfig,
    pub memory: MemoryConfig,
    pub compaction: CompactionConfig,
    pub permissions: PermissionConfig,
}

impl Features {
    /// Check whether a specific feature is enabled.
    pub fn is_enabled(&self, feature: Feature) -> bool {
        match feature {
            Feature::Skills => self.skills.enabled,
            Feature::Memory => self.memory.enabled,
            Feature::Compaction => self.compaction.enabled,
            Feature::Permissions => self.permissions.enabled,
        }
    }
}

// ── Defaults: everything ON ─────────────────────────────────────────────────

impl Default for SkillConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directories: Vec::new(), // resolved at runtime to ~/.flint/skills + .flint/skills
            active: Vec::new(),      // empty = all
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for Features {
    fn default() -> Self {
        Self {
            skills: SkillConfig::default(),
            memory: MemoryConfig::default(),
            compaction: CompactionConfig::default(),
            permissions: PermissionConfig::default(),
        }
    }
}
