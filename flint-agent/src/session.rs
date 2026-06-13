//! Conversation session management.

use anyhow::Result;
use flint_types::{ContentBlock, Message, ToolCall, ToolOutput};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// Session metadata for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub provider: String,
    pub model: String,
    pub title: String,
    pub message_count: usize,
}

/// Serializable session format.
#[derive(Debug, Serialize, Deserialize)]
struct SessionFile {
    meta: SessionMeta,
    messages: Vec<Message>,
}

/// A conversation session holding message history.
pub struct Session {
    pub messages: Vec<Message>,
    /// Circuit breaker: tracks consecutive errors from the same tool across turns.
    pub circuit_breaker_last_tool: Option<String>,
    /// Circuit breaker: count of consecutive errors from the same tool.
    pub circuit_breaker_count: u32,
}

impl Session {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            circuit_breaker_last_tool: None,
            circuit_breaker_count: 0,
        }
    }

    /// Add a user message.
    pub fn add_user(&mut self, text: &str) {
        self.messages.push(Message::user(text));
    }

    /// Add an assistant text message.
    pub fn add_assistant(&mut self, text: &str) {
        self.messages.push(Message::assistant(text));
    }

    /// Add an assistant message with tool use blocks.
    pub fn add_assistant_with_tools(&mut self, text: &str, tool_calls: &[ToolCall]) {
        let tool_uses: Vec<ContentBlock> = tool_calls
            .iter()
            .map(|tc| ContentBlock::ToolUse {
                id: tc.id.clone(),
                name: tc.name.clone(),
                input: tc.input.clone(),
            })
            .collect();
        self.messages
            .push(Message::assistant_with_tools(text, tool_uses));
    }

    /// Add a tool result message.
    pub fn add_tool_result(&mut self, tool_use_id: &str, output: &ToolOutput) {
        self.messages
            .push(Message::tool_result(tool_use_id, output));
    }

    /// Get message count.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Save session to file.
    pub fn save(&self, path: &Path, provider: &str, model: &str) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let title = self.extract_title();

        let meta = SessionMeta {
            id: id.clone(),
            created_at: now.clone(),
            updated_at: now,
            provider: provider.to_string(),
            model: model.to_string(),
            title,
            message_count: self.messages.len(),
        };

        let file = SessionFile {
            meta,
            messages: self.messages.clone(),
        };

        let json = serde_json::to_string_pretty(&file)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load session from file.
    pub fn load(path: &Path) -> Result<(Self, SessionMeta)> {
        let json = std::fs::read_to_string(path)?;
        let file: SessionFile = serde_json::from_str(&json)?;
        let session = Self {
            messages: file.messages,
            circuit_breaker_last_tool: None,
            circuit_breaker_count: 0,
        };
        Ok((session, file.meta))
    }

    /// List all sessions in a directory.
    pub fn list_sessions(dir: &Path) -> Result<Vec<SessionMeta>> {
        let mut sessions = Vec::new();
        if !dir.exists() {
            return Ok(sessions);
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                if let Ok((_, meta)) = Self::load(&path) {
                    sessions.push(meta);
                }
            }
        }

        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    /// Extract a title from the first user message.
    fn extract_title(&self) -> String {
        for msg in &self.messages {
            if msg.role == flint_types::Role::User {
                let text = msg.text();
                let title: String = text.chars().take(50).collect();
                if title.len() < text.len() {
                    return format!("{}...", title);
                }
                return title;
            }
        }
        "Empty session".to_string()
    }

    /// Update session file with new messages.
    pub fn update_save(&self, path: &Path, meta: &SessionMeta) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let updated_meta = SessionMeta {
            updated_at: now,
            message_count: self.messages.len(),
            ..meta.clone()
        };

        let file = SessionFile {
            meta: updated_meta,
            messages: self.messages.clone(),
        };

        let json = serde_json::to_string_pretty(&file)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
