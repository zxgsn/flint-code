//! Modular configuration for flint.
//!
//! All features are enabled by default. Users can selectively disable
//! features via `~/.flint/config.toml` (user-level) or `.flint.toml`
//! (project-level).
//!
//! ```toml
//! # .flint.toml — disable skills
//! [features.skills]
//! enabled = false
//! ```

pub mod config;
pub mod features;
pub mod skill;

pub use config::{AgentConfig, Config, LoggingConfig, McpServerConfig, ProviderConfig, SessionConfig, load};
pub use features::{AgentProfile, AutoPokeConfig, Feature, Features, SwarmConfig};
pub use skill::{Skill, SkillMeta};
