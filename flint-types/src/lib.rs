//! Core types for the flint agent harness.

use serde::{Deserialize, Serialize};

// ── Messages ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn system(text: &str) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    pub fn user(text: &str) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    pub fn assistant(text: &str) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    pub fn assistant_with_tools(text: &str, tool_uses: Vec<ContentBlock>) -> Self {
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(ContentBlock::Text {
                text: text.to_string(),
            });
        }
        content.extend(tool_uses);
        Self {
            role: Role::Assistant,
            content,
        }
    }

    pub fn tool_result(tool_use_id: &str, output: &ToolOutput) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: output.text.clone(),
                is_error: if output.is_error {
                    Some(true)
                } else {
                    None
                },
            }],
        }
    }

    /// Extract plain text from the message.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Estimate total character count including all content blocks
    /// (text, tool use inputs, tool result contents).
    /// This is more accurate than `text()` for context window management.
    pub fn estimated_chars(&self) -> usize {
        self.content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.len(),
                ContentBlock::ToolUse { id, name, input } => {
                    id.len() + name.len() + input.to_string().len()
                }
                ContentBlock::ToolResult {
                    tool_use_id, content, ..
                } => tool_use_id.len() + content.len(),
            })
            .sum()
    }
}

// ── Tools ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

// ── Streaming ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Incremental text from the assistant.
    TextDelta(String),
    /// A complete tool call (collected from deltas).
    ToolCall(ToolCall),
    /// Stream finished.
    End,
    /// Raw event for provider-specific debugging.
    Raw(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}
