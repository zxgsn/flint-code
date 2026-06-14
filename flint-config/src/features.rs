//! Feature toggles — all enabled by default.

use serde::{Deserialize, Serialize};

/// Individual feature identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Provider,
    Skills,
    Memory,
    Compaction,
    Permissions,
    Swarm,
    AutoPoke,
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

/// Per-agent model slot — assigns a specific model to agent 1, 2, 3, etc.
/// Position in the list determines the slot number (0-indexed).
/// Empty model string means "use default".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    #[serde(default)]
    pub model: String,
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
    /// Model override for sub-agents. None = inherit parent model.
    #[serde(default)]
    pub model: Option<String>,
    /// Default spawn mode: "terminal" or "in-process".
    #[serde(default = "default_spawn_mode")]
    pub spawn_mode: String,
    /// Named agent profiles — each defines a model for a specific role.
    /// Referenced via `profile="name"` in swarm spawn.
    #[serde(default)]
    pub agents: Vec<AgentProfile>,
    /// Model selection strategy: "auto" (agent decides freely),
    /// "profiles_only" (must pick from profiles), or "fixed" (always use default).
    #[serde(default = "default_model_selection")]
    pub model_selection: String,
}

// ── Aggregate feature container ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoPokeConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Features {
    pub skills: SkillConfig,
    pub memory: MemoryConfig,
    pub compaction: CompactionConfig,
    pub permissions: PermissionConfig,
    #[serde(default)]
    pub swarm: SwarmConfig,
    #[serde(default)]
    pub auto_poke: AutoPokeConfig,
}

impl Features {
    /// Check whether a specific feature is enabled.
    pub fn is_enabled(&self, feature: Feature) -> bool {
        match feature {
            Feature::Provider => false, // not a toggle
            Feature::Skills => self.skills.enabled,
            Feature::Memory => self.memory.enabled,
            Feature::Compaction => self.compaction.enabled,
            Feature::Permissions => self.permissions.enabled,
            Feature::Swarm => self.swarm.enabled,
            Feature::AutoPoke => self.auto_poke.enabled,
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

fn default_spawn_mode() -> String {
    "terminal".to_string()
}

fn default_model_selection() -> String {
    "auto".to_string()
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
            model: None,
            spawn_mode: default_spawn_mode(),
            agents: Vec::new(),
            model_selection: default_model_selection(),
        }
    }
}

impl Default for AutoPokeConfig {
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
            swarm: SwarmConfig::default(),
            auto_poke: AutoPokeConfig::default(),
        }
    }
}
