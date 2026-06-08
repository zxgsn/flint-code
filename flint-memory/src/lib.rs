//! flint-memory — Layered memory system for the flint agent harness.
//!
//! Three-layer architecture inspired by Letta/MemGPT:
//!
//! - **Core Memory** (Layer 1): Always-in-context blocks injected into the system
//!   prompt. The agent can read and write these via tool calls. Stores user
//!   preferences, persona, and project context.
//!
//! - **Archival Memory** (Layer 2): Long-term searchable knowledge store.
//!   Facts, corrections, patterns, and preferences persisted to disk.
//!   Retrieved on demand via keyword search.
//!
//! - **Recall Memory** (Layer 3): Session-local extracted facts held in memory
//!   during the current conversation. Not persisted across sessions.
//!
//! Storage: File-based JSON under `~/.flint/memory/`. No external database
//! dependencies. Two scopes: global (user-level) and project (per working dir).

pub mod core;
pub mod manager;
pub mod search;
pub mod store;
pub mod types;

pub use manager::{MemoryConfig, MemoryManager};
pub use types::{
    CoreBlock, ExtractedMemory, MemoryCategory, MemoryEntry, MemoryScope, RecallEntry, TrustLevel,
};
