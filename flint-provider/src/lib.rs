//! LLM provider abstraction for flint.
//!
//! Each provider implements the `Provider` trait, returning an async stream
//! of `StreamEvent`s. The agent loop is provider-agnostic.

pub mod anthropic;
pub mod openai;

use async_trait::async_trait;
use flint_types::{Message, StreamEvent, ToolDefinition};
use std::pin::Pin;
use std::time::Duration;

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

/// Wrapper that adds retry with exponential backoff to any Provider.
///
/// Retries on transient errors (network, 429 rate limit, 5xx server errors)
/// up to `max_retries` times with exponential backoff.
pub struct RetryProvider {
    inner: Box<dyn Provider>,
    max_retries: u32,
    base_delay: Duration,
}

impl RetryProvider {
    pub fn new(inner: Box<dyn Provider>) -> Self {
        Self {
            inner,
            max_retries: 3,
            base_delay: Duration::from_millis(1000),
        }
    }

    /// Whether an error is transient and worth retrying.
    fn is_transient(err: &anyhow::Error) -> bool {
        let msg = err.to_string().to_lowercase();
        // Rate limit
        if msg.contains("429") || msg.contains("rate limit") || msg.contains("too many requests")
        {
            return true;
        }
        // Server errors
        if msg.contains("500") || msg.contains("502") || msg.contains("503") {
            return true;
        }
        // Network errors
        if msg.contains("timeout")
            || msg.contains("connection")
            || msg.contains("eof")
            || msg.contains("broken pipe")
            || msg.contains("decode")
            || msg.contains("error decoding response body")
        {
            return true;
        }
        false
    }

    /// Extract retry-after duration from error message if present.
    fn retry_after(err: &anyhow::Error) -> Option<Duration> {
        let msg = err.to_string();
        // Look for "retry-after: N" or "Retry-After: N" in error
        for part in msg.split(|c: char| c == ',' || c == ';' || c == '\n') {
            let part = part.trim().to_lowercase();
            if let Some(val) = part.strip_prefix("retry-after:") {
                if let Ok(secs) = val.trim().parse::<u64>() {
                    return Some(Duration::from_secs(secs.min(60)));
                }
            }
        }
        None
    }
}

#[async_trait]
impl Provider for RetryProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
    ) -> anyhow::Result<EventStream> {
        let mut last_err = None;

        for attempt in 0..=self.max_retries {
            match self.inner.complete(messages, tools, system).await {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    if attempt < self.max_retries && Self::is_transient(&err) {
                        let delay = Self::retry_after(&err).unwrap_or_else(|| {
                            self.base_delay * 2u32.pow(attempt)
                        });
                        tracing::warn!(
                            "provider attempt {} failed ({}), retrying in {:?}",
                            attempt + 1,
                            err,
                            delay
                        );
                        tokio::time::sleep(delay).await;
                        last_err = Some(err);
                    } else {
                        return Err(err);
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all retry attempts failed")))
    }
}
