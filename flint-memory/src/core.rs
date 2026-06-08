//! Core Memory — Layer 1: always-in-context blocks.
//!
//! Core memory blocks are injected into the system prompt on every turn.
//! Inspired by Letta's Block concept: the agent can read and write its own
//! core memory via tool calls.
//!
//! Default blocks:
//! - `persona`: who the agent is and how it should behave
//! - `user`: information about the user
//! - `project`: current project context and goals

use anyhow::{Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};

use crate::types::CoreBlock;

/// Manages core memory blocks with file-based persistence.
pub struct CoreMemory {
    blocks: Vec<CoreBlock>,
    path: PathBuf,
}

impl CoreMemory {
    /// Load core memory from file, or create with defaults.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let blocks: Vec<CoreBlock> = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            Ok(Self {
                blocks,
                path: path.to_path_buf(),
            })
        } else {
            // Create parent directory
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let blocks = default_blocks();
            let mem = Self {
                blocks,
                path: path.to_path_buf(),
            };
            mem.save()?;
            Ok(mem)
        }
    }

    /// Save core memory to file.
    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.blocks)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    /// Get all blocks.
    pub fn blocks(&self) -> &[CoreBlock] {
        &self.blocks
    }

    /// Get a block by label.
    pub fn get(&self, label: &str) -> Option<&CoreBlock> {
        self.blocks.iter().find(|b| b.label == label)
    }

    /// Get a mutable block by label.
    pub fn get_mut(&mut self, label: &str) -> Option<&mut CoreBlock> {
        self.blocks.iter_mut().find(|b| b.label == label)
    }

    /// Update a block's content. Creates the block if it doesn't exist.
    /// Returns true if the update was applied.
    pub fn update(&mut self, label: &str, content: &str) -> Result<bool> {
        if let Some(block) = self.blocks.iter_mut().find(|b| b.label == label) {
            if block.read_only {
                return Ok(false);
            }
            if content.len() > block.limit {
                return Ok(false);
            }
            block.content = content.to_string();
            block.updated_at = Utc::now();
            self.save()?;
            return Ok(true);
        }
        // Block doesn't exist — create it
        self.blocks.push(CoreBlock::new(label, content));
        self.save()?;
        Ok(true)
    }

    /// Insert text into an existing block at a position.
    /// Returns true if the insert was applied.
    pub fn insert(&mut self, label: &str, content: &str) -> Result<bool> {
        if let Some(block) = self.blocks.iter_mut().find(|b| b.label == label) {
            if block.read_only {
                return Ok(false);
            }
            let new_content = if block.content.is_empty() {
                content.to_string()
            } else {
                format!("{}\n{}", block.content, content)
            };
            if new_content.len() > block.limit {
                return Ok(false);
            }
            block.content = new_content;
            block.updated_at = Utc::now();
            self.save()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Append a new block.
    pub fn add_block(&mut self, block: CoreBlock) -> Result<()> {
        self.blocks.push(block);
        self.save()
    }

    /// Remove a block by label (unless read-only).
    pub fn remove_block(&mut self, label: &str) -> Result<bool> {
        if let Some(pos) = self.blocks.iter().position(|b| b.label == label) {
            if self.blocks[pos].read_only {
                return Ok(false);
            }
            self.blocks.remove(pos);
            self.save()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Render all core memory blocks for inclusion in the system prompt.
    pub fn render_for_prompt(&self) -> String {
        if self.blocks.is_empty() {
            return String::new();
        }

        let mut output = String::from("\n\n## Memory (Core)\n");
        output.push_str("The following is your core memory — always available context.\n");
        output.push_str("You can update these blocks using the memory_update_core tool.\n\n");

        for block in &self.blocks {
            output.push_str(&block.render());
            output.push('\n');
        }

        output
    }
}

/// Default core memory blocks for a new installation.
fn default_blocks() -> Vec<CoreBlock> {
    vec![
        CoreBlock::new("persona", "You are flint, a fast and focused coding agent. You write clean, efficient code and communicate concisely."),
        CoreBlock::new("user", "(No information about the user yet. Learn preferences through interaction.)"),
        CoreBlock::new("project", "(No project context loaded yet. Discover through exploration.)"),
    ]
}
