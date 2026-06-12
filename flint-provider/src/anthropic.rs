//! Anthropic Messages API provider with SSE streaming.

use anyhow::{bail, Result};
use async_trait::async_trait;
use flint_types::{ContentBlock, Message, Role, StreamEvent, ToolCall, ToolDefinition};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::{EventStream, Provider};

// ── Config ────────────────────────────────────────────────────────────────

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    max_tokens: u32,
    /// When true, send `Authorization: Bearer <key>` instead of `x-api-key: <key>`.
    /// Used when the key originates from an AUTH_TOKEN env var (proxy / compatible endpoints).
    use_bearer_auth: bool,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                // No read_timeout — SSE streaming can have long gaps between chunks
                .build()
                .expect("failed to build HTTP client"),
            api_key: api_key.into(),
            model: model.into(),
            base_url: "https://api.anthropic.com".to_string(),
            max_tokens: 8192,
            use_bearer_auth: false,
        }
    }

    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    /// Use `Authorization: Bearer` header instead of `x-api-key`.
    /// Call this when the key comes from an AUTH_TOKEN env var.
    pub fn bearer_auth(mut self, enabled: bool) -> Self {
        self.use_bearer_auth = enabled;
        self
    }
}

// ── Request types ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "str::is_empty")]
    system: &'a str,
    messages: &'a [ApiMessage],
    tools: &'a [ApiTool<'a>],
    stream: bool,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: Vec<ApiContent>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiContent {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}

#[derive(Serialize)]
struct ApiTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a serde_json::Value,
}

// ── SSE event types ───────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseEvent {
    #[allow(dead_code)]
    MessageStart { message: MessageStart },
    ContentBlockStart { index: usize, content_block: ContentBlockStart },
    ContentBlockDelta { index: usize, delta: Delta },
    ContentBlockStop { index: usize },
    #[allow(dead_code)]
    MessageDelta { delta: MessageDeltaInner },
    MessageStop,
    #[allow(dead_code)]
    Ping,
    Error { error: ErrorDetail },
}

#[derive(Deserialize, Debug)]
struct MessageStart {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    model: String,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlockStart {
    Text,
    ToolUse { id: String, name: String },
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Delta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
}

#[derive(Deserialize, Debug)]
struct MessageDeltaInner {
    #[allow(dead_code)]
    stop_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ErrorDetail {
    message: String,
}

// ── Conversion: internal Message → API format ─────────────────────────────

fn to_api_messages(messages: &[Message]) -> Vec<ApiMessage> {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "user",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "user",
            };
            let content = m
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => ApiContent::Text { text: text.clone() },
                    ContentBlock::ToolUse { id, name, input } => {
                        ApiContent::ToolUse { id: id.clone(), name: name.clone(), input: input.clone() }
                    }
                    ContentBlock::ToolResult {
                        tool_use_id, content, is_error,
                    } => ApiContent::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: content.clone(),
                        is_error: is_error.unwrap_or(false),
                    },
                })
                .collect();
            ApiMessage { role: role.to_string(), content }
        })
        .collect()
}

fn to_api_tools(tools: &[ToolDefinition]) -> Vec<ApiTool<'_>> {
    tools
        .iter()
        .map(|t| ApiTool {
            name: &t.name,
            description: &t.description,
            input_schema: &t.parameters,
        })
        .collect()
}

// ── Provider impl ─────────────────────────────────────────────────────────

#[async_trait]
impl Provider for AnthropicProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
    ) -> Result<EventStream> {
        let api_messages = to_api_messages(messages);
        let api_tools = to_api_tools(tools);

        let body = Request {
            model: &self.model,
            max_tokens: self.max_tokens,
            system,
            messages: &api_messages,
            tools: &api_tools,
            stream: true,
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        if self.use_bearer_auth {
            let bearer = format!("Bearer {}", self.api_key);
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&bearer)
                    .map_err(|_| anyhow::anyhow!("invalid API key"))?,
            );
        } else {
            headers.insert(
                "x-api-key",
                HeaderValue::from_str(&self.api_key)
                    .map_err(|_| anyhow::anyhow!("invalid API key"))?,
            );
        }

        // If base_url already contains a path (e.g. proxy with /v1/chat/completions), use as-is
        let url = if self.base_url.contains("/v1/") {
            self.base_url.clone()
        } else {
            format!("{}/v1/messages", self.base_url)
        };
        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Anthropic API error {}: {}", status, text);
        }

        // Collect raw SSE events from the byte stream
        // Buffer partial lines across chunks to handle split SSE events
        let byte_stream = resp.bytes_stream();
        let mut line_buf = String::new();
        let sse_stream = byte_stream
            .map(move |chunk| {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => return vec![Err(anyhow::anyhow!(e))],
                };
                let text = String::from_utf8_lossy(&chunk);
                line_buf.push_str(&text);

                let mut events = Vec::new();
                // Process all complete lines in the buffer
                while let Some(newline_pos) = line_buf.find('\n') {
                    let line = line_buf[..newline_pos].trim_end_matches('\r').to_string();
                    line_buf.drain(..=newline_pos);

                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            continue;
                        }
                        match serde_json::from_str::<SseEvent>(data) {
                            Ok(event) => events.push(Ok(event)),
                            Err(_) => continue,
                        }
                    }
                }
                events
            })
            .flat_map(|events| futures::stream::iter(events));

        // Box the source stream so it can be moved into unfold state
        let mut source: std::pin::Pin<Box<dyn futures::Stream<Item = Result<SseEvent>> + Send>> =
            Box::pin(sse_stream);

        // Accumulate tool calls across content blocks
        let mut tools_acc: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();

        let mut done = false;
        let output = futures::stream::poll_fn(move |cx| {
            if done {
                return std::task::Poll::Ready(None);
            }
            loop {
                match source.as_mut().poll_next(cx) {
                    std::task::Poll::Ready(Some(Ok(event))) => match event {
                        SseEvent::ContentBlockDelta { index, delta } => match delta {
                            Delta::TextDelta { text } => {
                                return std::task::Poll::Ready(Some(Ok(StreamEvent::TextDelta(text))));
                            }
                            Delta::InputJsonDelta { partial_json } => {
                                if let Some((_, _, buf)) = tools_acc.get_mut(&index) {
                                    buf.push_str(&partial_json);
                                }
                                continue;
                            }
                        },
                        SseEvent::ContentBlockStart { index, content_block } => {
                            if let ContentBlockStart::ToolUse { id, name } = content_block {
                                tools_acc.insert(index, (id, name, String::new()));
                            }
                            continue;
                        }
                        SseEvent::ContentBlockStop { index } => {
                            if let Some((id, name, json_buf)) = tools_acc.remove(&index) {
                                let input: serde_json::Value =
                                    serde_json::from_str(&json_buf).unwrap_or(serde_json::json!({}));
                                return std::task::Poll::Ready(Some(Ok(StreamEvent::ToolCall(
                                    ToolCall { id, name, input },
                                ))));
                            }
                            continue;
                        }
                        SseEvent::MessageStop => {
                            done = true;
                            return std::task::Poll::Ready(Some(Ok(StreamEvent::End)));
                        }
                        SseEvent::Error { error } => {
                            done = true;
                            return std::task::Poll::Ready(Some(Err(
                                anyhow::anyhow!("Anthropic error: {}", error.message),
                            )));
                        }
                        _ => continue,
                    },
                    std::task::Poll::Ready(Some(Err(e))) => {
                        done = true;
                        return std::task::Poll::Ready(Some(Err(e)));
                    }
                    std::task::Poll::Ready(None) => {
                        done = true;
                        return std::task::Poll::Ready(Some(Ok(StreamEvent::End)));
                    }
                    std::task::Poll::Pending => return std::task::Poll::Pending,
                }
            }
        });

        Ok(Box::pin(output))
    }
}
