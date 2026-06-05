//! LLM provider abstraction for flint.
//!
//! Each provider implements the `Provider` trait, returning an async stream
//! of `StreamEvent`s. The agent loop is provider-agnostic.

pub mod anthropic;
pub mod openai;

use async_trait::async_trait;
use flint_types::{Message, StreamEvent, ToolDefinition};
use std::pin::Pin;

pub type EventStream = Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamEvent>> + Send>>;

/// Unified LLM provider interface.
///
/// Implement this trait to add a new backend. The agent loop calls
/// `complete()` once per turn; the stream yields `TextDelta` for display,
/// `ToolCall` for tool execution, and `End` when done.
#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
    ) -> anyhow::Result<EventStream>;
}
