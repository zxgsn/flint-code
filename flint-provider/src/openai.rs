//! OpenAI Chat Completions API provider with SSE streaming.

use anyhow::{bail, Result};
use async_trait::async_trait;
use flint_types::{ContentBlock, Message, Role, StreamEvent, ToolCall, ToolDefinition};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::{EventStream, Provider};

// ── Config ────────────────────────────────────────────────────────────────

pub struct OpenAIProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    max_tokens: u32,
}

impl OpenAIProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                // No read_timeout — SSE streaming can have long gaps between chunks
                .build()
                .expect("failed to build HTTP client"),
            api_key: api_key.into(),
            model: model.into(),
            base_url: "https://api.openai.com/v1".to_string(),
            max_tokens: 8192,
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
}

// ── Request types ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: &'a [ApiMessage],
    tools: &'a [ApiTool<'a>],
    stream: bool,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ApiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
struct ApiToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: &'static str,
    function: ApiFunction,
}

#[derive(Serialize)]
struct ApiFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ApiTool<'a> {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: ApiToolFunction<'a>,
}

#[derive(Serialize)]
struct ApiToolFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

// ── SSE response types ────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct SseResponse {
    choices: Vec<SseChoice>,
}

#[derive(Deserialize, Debug)]
struct SseChoice {
    delta: Option<SseDelta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct SseDelta {
    content: Option<String>,
    tool_calls: Option<Vec<SseToolCallDelta>>,
}

#[derive(Deserialize, Debug)]
struct SseToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<SseFunctionDelta>,
}

#[derive(Deserialize, Debug)]
struct SseFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

// ── Conversion ────────────────────────────────────────────────────────────

fn to_api_messages(messages: &[Message]) -> Vec<ApiMessage> {
    let mut result = Vec::new();
    for m in messages {
        match m.role {
            Role::System => {
                result.push(ApiMessage {
                    role: "system".to_string(),
                    content: Some(m.text()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            Role::User => {
                result.push(ApiMessage {
                    role: "user".to_string(),
                    content: Some(m.text()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            Role::Assistant => {
                let text = m.text();
                let tool_calls: Vec<ApiToolCall> = m
                    .content
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolUse { id, name, input } = b {
                            Some(ApiToolCall {
                                id: id.clone(),
                                call_type: "function",
                                function: ApiFunction {
                                    name: name.clone(),
                                    arguments: input.to_string(),
                                },
                            })
                        } else {
                            None
                        }
                    })
                    .collect();
                result.push(ApiMessage {
                    role: "assistant".to_string(),
                    content: if text.is_empty() { None } else { Some(text) },
                    tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                    tool_call_id: None,
                });
            }
            Role::Tool => {
                for b in &m.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id, content, ..
                    } = b
                    {
                        result.push(ApiMessage {
                            role: "tool".to_string(),
                            content: Some(content.clone()),
                            tool_calls: None,
                            tool_call_id: Some(tool_use_id.clone()),
                        });
                    }
                }
            }
        }
    }
    result
}

fn to_api_tools(tools: &[ToolDefinition]) -> Vec<ApiTool<'_>> {
    tools
        .iter()
        .map(|t| ApiTool {
            tool_type: "function",
            function: ApiToolFunction {
                name: &t.name,
                description: &t.description,
                parameters: &t.parameters,
            },
        })
        .collect()
}

// ── Provider impl ─────────────────────────────────────────────────────────

#[async_trait]
impl Provider for OpenAIProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
    ) -> Result<EventStream> {
        let mut all_messages = Vec::new();
        if !system.is_empty() {
            all_messages.push(ApiMessage {
                role: "system".to_string(),
                content: Some(system.to_string()),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        all_messages.extend(to_api_messages(messages));

        let api_tools = to_api_tools(tools);

        let body = Request {
            model: &self.model,
            max_tokens: self.max_tokens,
            messages: &all_messages,
            tools: &api_tools,
            stream: true,
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .map_err(|_| anyhow::anyhow!("invalid API key"))?,
        );

        let url = format!("{}/chat/completions", self.base_url);
        let resp = self.client.post(&url).headers(headers).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("OpenAI API error {}: {}", status, text);
        }

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
                while let Some(newline_pos) = line_buf.find('\n') {
                    let line = line_buf[..newline_pos].trim_end_matches('\r').to_string();
                    line_buf.drain(..=newline_pos);

                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            continue;
                        }
                        match serde_json::from_str::<SseResponse>(data) {
                            Ok(resp) => events.push(Ok(resp)),
                            Err(_) => continue,
                        }
                    }
                }
                events
            })
            .flat_map(|events| futures::stream::iter(events));

        let mut source: std::pin::Pin<Box<dyn futures::Stream<Item = Result<SseResponse>> + Send>> =
            Box::pin(sse_stream);

        let mut tools_acc: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();
        let mut pending: Vec<StreamEvent> = Vec::new();
        let mut done = false;

        let output = futures::stream::poll_fn(move |cx| {
            // Drain pending events first (tool calls accumulated from previous poll)
            if let Some(evt) = pending.pop() {
                return std::task::Poll::Ready(Some(Ok(evt)));
            }

            if done {
                return std::task::Poll::Ready(None);
            }

            loop {
                match source.as_mut().poll_next(cx) {
                    std::task::Poll::Ready(Some(Ok(resp))) => {
                        if let Some(choice) = resp.choices.first() {
                            if let Some(delta) = &choice.delta {
                                // Text content
                                if let Some(content) = &delta.content {
                                    return std::task::Poll::Ready(Some(Ok(
                                        StreamEvent::TextDelta(content.clone()),
                                    )));
                                }
                                // Tool call deltas
                                if let Some(tc_deltas) = &delta.tool_calls {
                                    for tc in tc_deltas {
                                        if let Some(id) = &tc.id {
                                            tools_acc.insert(tc.index, (id.clone(), String::new(), String::new()));
                                        }
                                        if let Some(func) = &tc.function {
                                            if let Some(name) = &func.name {
                                                if let Some((_, n, _)) = tools_acc.get_mut(&tc.index) {
                                                    *n = name.clone();
                                                }
                                            }
                                            if let Some(args) = &func.arguments {
                                                if let Some((_, _, buf)) = tools_acc.get_mut(&tc.index) {
                                                    buf.push_str(args);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // finish_reason = "tool_calls" → flush accumulated tool calls
                            if choice.finish_reason.as_deref() == Some("tool_calls") {
                                let calls: Vec<_> = tools_acc.drain().collect();
                                // Push tool calls as pending events (in reverse so pop() gives correct order)
                                for (_, (id, name, json_buf)) in calls.into_iter().rev() {
                                    let input: serde_json::Value =
                                        serde_json::from_str(&json_buf).unwrap_or(serde_json::json!({}));
                                    pending.push(StreamEvent::ToolCall(ToolCall { id, name, input }));
                                }
                                // First tool call goes out now, rest via pending
                                if let Some(evt) = pending.pop() {
                                    return std::task::Poll::Ready(Some(Ok(evt)));
                                }
                                continue;
                            }
                            if choice.finish_reason.as_deref() == Some("stop") {
                                done = true;
                                return std::task::Poll::Ready(Some(Ok(StreamEvent::End)));
                            }
                        }
                        continue;
                    }
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
