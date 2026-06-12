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
    Swarm,
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
    /// Maximum number of core memory blocks.
    #[serde(default = "default_max_core_blocks")]
    pub max_core_blocks: usize,
    /// Character limit per core memory block.
    #[serde(default = "default_max_block_chars")]
    pub max_block_chars: usize,
    /// Whether to auto-extract facts after each turn.
    #[serde(default = "default_true")]
    pub auto_extract: bool,
    /// Maximum number of memories to inject into context per turn.
    #[serde(default = "default_search_limit")]
    pub search_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    pub enabled: bool,
    /// Maximum number of concurrent sub-agents.
    #[serde(default = "default_max_agents")]
    pub max_agents: usize,
    /// Max LLM turns per sub-agent task.
    #[serde(default = "default_agent_max_turns")]
    pub agent_max_turns: u32,
    /// Task timeout in seconds.
    #[serde(default = "default_task_timeout_secs")]
    pub task_timeout_secs: u64,
}

// ── Aggregate feature container ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Features {
    pub skills: SkillConfig,
    pub memory: MemoryConfig,
    pub compaction: CompactionConfig,
    pub permissions: PermissionConfig,
    #[serde(default)]
    pub swarm: SwarmConfig,
}

impl Features {
    /// Check whether a specific feature is enabled.
    pub fn is_enabled(&self, feature: Feature) -> bool {
        match feature {
            Feature::Skills => self.skills.enabled,
            Feature::Memory => self.memory.enabled,
            Feature::Compaction => self.compaction.enabled,
            Feature::Permissions => self.permissions.enabled,
            Feature::Swarm => self.swarm.enabled,
        }
    }
}

// ── Default value helpers ─────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

fn default_max_core_blocks() -> usize {
    8
}

fn default_max_block_chars() -> usize {
    2000
}

fn default_search_limit() -> usize {
    5
}

fn default_max_agents() -> usize {
    5
}

fn default_agent_max_turns() -> u32 {
    20
}

fn default_task_timeout_secs() -> u64 {
    300
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
        Self {
            enabled: true,
            max_core_blocks: default_max_core_blocks(),
            max_block_chars: default_max_block_chars(),
            auto_extract: true,
            search_limit: default_search_limit(),
        }
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

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            enabled: false, // opt-in
            max_agents: default_max_agents(),
            agent_max_turns: default_agent_max_turns(),
            task_timeout_secs: default_task_timeout_secs(),
        }
    }
}

impl Default for Features {
    fn default() -> Self {
        Self {
            skills: SkillConfig::default(),
            memory: MemoryConfig::default(),
            compaction: CompactionConfig::default(),
            permissions: PermissionConfig::default(),
            swarm: SwarmConfig::default(),
        }
    }
}
