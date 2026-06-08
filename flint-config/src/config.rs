//! Configuration loading and merging.
//!
//! Priority (highest wins):
//! 1. CLI arguments (applied by the caller)
//! 2. Project-level `.flint.toml`
//! 3. User-level `~/.flint/config.toml`
//! 4. Built-in defaults

use crate::features::Features;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Config sections ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// "anthropic" or "openai"
    #[serde(default = "default_provider_type")]
    pub r#type: String,
    /// Model identifier
    #[serde(default = "default_model")]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Override the default system prompt.
    pub system_prompt: Option<String>,
    /// Maximum tool-call turns per user message.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    /// Truncate tool output beyond this many characters.
    #[serde(default = "default_max_output_chars")]
    pub max_output_chars: usize,
    /// Approximate context window size in characters (~4 chars per token).
    /// Used for auto-compaction. Default 500000 (~125k tokens).
    #[serde(default = "default_context_window_chars")]
    pub context_window_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Whether to persist sessions across restarts.
    #[serde(default = "default_true")]
    pub persistence: bool,
    /// Directory for session files.
    #[serde(default = "default_session_path")]
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// trace | debug | info | warn | error
    #[serde(default = "default_log_level")]
    pub level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Command to spawn the MCP server process.
    pub command: String,
    /// Arguments to pass to the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables for the server process.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

// ── Top-level config ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub features: Features,
    /// MCP server configurations. Key is the server ID.
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

// ── Loading ─────────────────────────────────────────────────────────────────

/// Load configuration by merging user-level and project-level files.
///
/// Returns the merged config. Missing files are silently skipped.
pub fn load(project_root: Option<&Path>) -> anyhow::Result<Config> {
    let mut config = Config::default();

    // Layer 1: user-level (~/.flint/config.toml)
    if let Some(user_dir) = dirs::home_dir() {
        let user_config = user_dir.join(".flint").join("config.toml");
        if user_config.is_file() {
            merge_file(&mut config, &user_config)?;
            tracing::info!("loaded user config: {}", user_config.display());
        }
    }

    // Layer 2: project-level (.flint.toml in working directory)
    let project_config = match project_root {
        Some(root) => root.join(".flint.toml"),
        None => PathBuf::from(".flint.toml"),
    };
    if project_config.is_file() {
        merge_file(&mut config, &project_config)?;
        tracing::info!("loaded project config: {}", project_config.display());
    }

    // Layer 3: environment variables (provider-specific, handled by caller)

    Ok(config)
}

impl Config {
    /// Serialize the config to TOML and write to the given path.
    /// Creates parent directories if needed.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let toml = toml::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize config: {}", e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &toml)
            .map_err(|e| anyhow::anyhow!("failed to write {}: {}", path.display(), e))?;
        tracing::info!("config saved to {}", path.display());
        Ok(())
    }

    /// Determine the best save path: project-level `.flint.toml` if in a project,
    /// otherwise user-level `~/.flint/config.toml`.
    pub fn save_path(&self, project_root: Option<&Path>) -> PathBuf {
        // Prefer project-level config
        if let Some(root) = project_root {
            return root.join(".flint.toml");
        }
        // Fallback to user-level
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".flint")
            .join("config.toml")
    }

    /// Resolve skill directories with defaults.
    ///
    /// If `features.skills.directories` is empty, returns:
    /// - `~/.flint/skills` (user-level)
    /// - `<project_root>/skills` (project-level, if provided)
    pub fn skill_dirs(&self, project_root: Option<&Path>) -> Vec<PathBuf> {
        if !self.features.skills.directories.is_empty() {
            return self.features.skills.directories.clone();
        }

        let mut dirs = Vec::new();
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".flint").join("skills"));
        }
        if let Some(root) = project_root {
            dirs.push(root.join("skills"));
        }
        dirs
    }

    /// Load all skills from resolved directories, filtered by active list.
    ///
    /// If `features.skills.active` is empty, all loaded skills are returned.
    pub fn load_skills(&self, project_root: Option<&Path>) -> Vec<crate::skill::Skill> {
        let dirs = self.skill_dirs(project_root);
        let all = crate::skill::load_skills_from_dirs(&dirs);

        let active = &self.features.skills.active;
        if active.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|s| active.contains(&s.name))
                .collect()
        }
    }

    /// Load all skills (ignoring active filter), for display purposes.
    pub fn load_all_skills(&self, project_root: Option<&Path>) -> Vec<crate::skill::Skill> {
        let dirs = self.skill_dirs(project_root);
        crate::skill::load_skills_from_dirs(&dirs)
    }

    /// Load skill metadata (name + description only, no prompt body).
    /// Cheap to scan, used for listing and matching.
    pub fn load_skill_metas(&self, project_root: Option<&Path>) -> Vec<crate::skill::SkillMeta> {
        let dirs = self.skill_dirs(project_root);
        crate::skill::load_metas_from_dirs(&dirs)
    }

    /// Load a single skill by name from the resolved directories.
    pub fn load_skill_by_name(&self, name: &str, project_root: Option<&Path>) -> Option<crate::skill::Skill> {
        let dirs = self.skill_dirs(project_root);
        crate::skill::load_skill_by_name(name, &dirs)
    }

    /// Ensure all skill directories exist, creating them if needed.
    /// Returns paths of directories that were created.
    pub fn ensure_skill_dirs(&self, project_root: Option<&Path>) -> Vec<PathBuf> {
        let dirs = self.skill_dirs(project_root);
        let mut created = Vec::new();
        for dir in &dirs {
            if crate::skill::ensure_dir(dir).unwrap_or(false) {
                created.push(dir.clone());
            }
        }
        created
    }
}

/// Merge a TOML file into the existing config. Only non-default values override.
fn merge_file(config: &mut Config, path: &Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {}", path.display(), e))?;
    let partial: Config = toml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {}", path.display(), e))?;

    // Merge each section: if the file specifies a value, it overrides the default.
    merge_provider(&mut config.provider, &partial.provider);
    merge_agent(&mut config.agent, &partial.agent);
    merge_session(&mut config.session, &partial.session);
    merge_logging(&mut config.logging, &partial.logging);
    merge_features(&mut config.features, &partial.features);
    merge_mcp_servers(&mut config.mcp_servers, &partial.mcp_servers);

    Ok(())
}

fn merge_provider(target: &mut crate::config::ProviderConfig, source: &crate::config::ProviderConfig) {
    // Always merge provider config from file, even if values match defaults
    // This ensures user's explicit config is respected
    target.r#type = source.r#type.clone();
    target.model = source.model.clone();
}

fn merge_agent(target: &mut AgentConfig, source: &AgentConfig) {
    if source.system_prompt.is_some() {
        target.system_prompt = source.system_prompt.clone();
    }
    if source.max_turns != default_max_turns() {
        target.max_turns = source.max_turns;
    }
    if source.max_output_chars != default_max_output_chars() {
        target.max_output_chars = source.max_output_chars;
    }
}

fn merge_session(target: &mut SessionConfig, source: &SessionConfig) {
    if source.persistence != default_true() {
        target.persistence = source.persistence;
    }
    if source.path != default_session_path() {
        target.path = source.path.clone();
    }
}

fn merge_logging(target: &mut LoggingConfig, source: &LoggingConfig) {
    if source.level != default_log_level() {
        target.level = source.level.clone();
    }
}

fn merge_features(target: &mut Features, source: &Features) {
    // Features use serde(default) so partial TOML works naturally.
    // For each feature, only override if the source file explicitly set it.
    // Since we can't distinguish "not set" from "default" with serde alone,
    // we merge at the TOML value level instead.
    // For now, if the file has a [features] section, it fully replaces.
    // This is acceptable because users writing [features] intend to control it.
    *target = source.clone();
}

fn merge_mcp_servers(
    target: &mut HashMap<String, McpServerConfig>,
    source: &HashMap<String, McpServerConfig>,
) {
    // MCP servers from the source file are added/override target.
    // Servers only in target are preserved.
    for (k, v) in source {
        target.insert(k.clone(), v.clone());
    }
}

// ── Defaults ────────────────────────────────────────────────────────────────

fn default_provider_type() -> String {
    "openai".to_string()
}

fn default_model() -> String {
    "mimo-v2.5-pro".to_string()
}

fn default_max_turns() -> u32 {
    50
}

fn default_max_output_chars() -> usize {
    65536
}

fn default_context_window_chars() -> usize {
    500_000 // ~125k tokens
}

fn default_true() -> bool {
    true
}

fn default_session_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flint")
        .join("sessions")
}

fn default_log_level() -> String {
    "warn".to_string()
}

// ── Default impls ───────────────────────────────────────────────────────────

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            r#type: default_provider_type(),
            model: default_model(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            max_turns: default_max_turns(),
            max_output_chars: default_max_output_chars(),
            context_window_chars: default_context_window_chars(),
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            persistence: default_true(),
            path: default_session_path(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}
