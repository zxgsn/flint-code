//! Cross-agent session import support.
//!
//! Supports importing sessions from:
//! - Claude Code (JSONL format)
//! - Flint native format

use anyhow::Result;
use flint_agent::{Session, SessionMeta};
use flint_types::{ContentBlock, Message, Role, ToolOutput};
use serde::Deserialize;
use std::path::Path;

/// Supported agent formats.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentFormat {
    Flint,
    ClaudeCode,
    Unknown,
}

/// Claude Code JSONL event types.
#[derive(Debug, Deserialize)]
struct ClaudeEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    message: Option<ClaudeMessage>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Claude Code message format.
#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    role: String,
    content: serde_json::Value,
    #[serde(default)]
    model: Option<String>,
}

/// Detect session format from file.
pub fn detect_format(path: &Path) -> AgentFormat {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

    if extension == "json" && !filename.ends_with(".jsonl") {
        // Check if it's flint format
        if let Ok(content) = std::fs::read_to_string(path) {
            if content.contains("\"meta\"") && content.contains("\"messages\"") {
                return AgentFormat::Flint;
            }
        }
    }

    if extension == "jsonl" || filename.ends_with(".jsonl") {
        // Check if it's Claude Code format
        if let Ok(content) = std::fs::read_to_string(path) {
            if content.contains("\"type\":\"user\"") || content.contains("\"type\":\"assistant\"") {
                return AgentFormat::ClaudeCode;
            }
        }
    }

    AgentFormat::Unknown
}

/// Import session from file.
pub fn import_session(path: &Path) -> Result<(Session, SessionMeta)> {
    let format = detect_format(path);
    match format {
        AgentFormat::Flint => Session::load(path),
        AgentFormat::ClaudeCode => import_claude_code(path),
        AgentFormat::Unknown => Err(anyhow::anyhow!("Unknown session format")),
    }
}

/// Import Claude Code JSONL session.
fn import_claude_code(path: &Path) -> Result<(Session, SessionMeta)> {
    let content = std::fs::read_to_string(path)?;
    let mut messages = Vec::new();
    let mut session_id = String::new();
    let mut first_timestamp = String::new();
    let mut last_timestamp = String::new();
    let mut model = String::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let event: ClaudeEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Track session metadata
        if let Some(ref sid) = event.session_id {
            if session_id.is_empty() {
                session_id = sid.clone();
            }
        }
        if let Some(ref ts) = event.timestamp {
            if first_timestamp.is_empty() {
                first_timestamp = ts.clone();
            }
            last_timestamp = ts.clone();
        }

        // Extract messages
        if let Some(ref msg) = event.message {
            if msg.role == "user" {
                if let Some(text) = extract_text_content(&msg.content) {
                    messages.push(Message::user(&text));
                }
            } else if msg.role == "assistant" {
                if msg.model.is_some() && model.is_empty() {
                    model = msg.model.clone().unwrap_or_default();
                }

                let (text, tool_calls) = extract_assistant_content(&msg.content);
                if !tool_calls.is_empty() {
                    messages.push(Message::assistant_with_tools(&text, tool_calls));
                } else if !text.is_empty() {
                    messages.push(Message::assistant(&text));
                }
            } else if msg.role == "tool" {
                if let Some(tool_result) = extract_tool_result(&msg.content) {
                    messages.push(tool_result);
                }
            }
        }
    }

    if messages.is_empty() {
        return Err(anyhow::anyhow!("No messages found in session"));
    }

    let title = extract_title(&messages);
    let meta = SessionMeta {
        id: session_id,
        created_at: first_timestamp,
        updated_at: last_timestamp,
        provider: "claude-code".to_string(),
        model,
        title,
        message_count: messages.len(),
    };

    let session = Session { messages };
    Ok((session, meta))
}

/// Extract text from Claude Code content value.
fn extract_text_content(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(arr) => {
            let texts: Vec<String> = arr
                .iter()
                .filter_map(|block| {
                    if let Some(block_type) = block.get("type") {
                        if block_type == "text" {
                            block.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        _ => None,
    }
}

/// Extract assistant content (text + tool calls) from Claude Code format.
fn extract_assistant_content(content: &serde_json::Value) -> (String, Vec<ContentBlock>) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();

    if let Some(arr) = content.as_array() {
        for block in arr {
            if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
                match block_type {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                    }
                    "tool_use" => {
                        let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                        let input = block.get("input").cloned().unwrap_or(serde_json::Value::Null);
                        tool_calls.push(ContentBlock::ToolUse { id, name, input });
                    }
                    _ => {}
                }
            }
        }
    }

    (text, tool_calls)
}

/// Extract tool result from Claude Code format.
fn extract_tool_result(content: &serde_json::Value) -> Option<Message> {
    if let Some(arr) = content.as_array() {
        for block in arr {
            if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
                if block_type == "tool_result" {
                    let tool_use_id = block.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    let content_text = block.get("content").and_then(|c| {
                        if let Some(s) = c.as_str() {
                            Some(s.to_string())
                        } else if let Some(arr) = c.as_array() {
                            let texts: Vec<String> = arr.iter().filter_map(|item| {
                                item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                            }).collect();
                            Some(texts.join("\n"))
                        } else {
                            None
                        }
                    }).unwrap_or_default();

                    let is_error = block.get("is_error").and_then(|e| e.as_bool());
                    let output = if is_error.unwrap_or(false) {
                        ToolOutput::error(content_text)
                    } else {
                        ToolOutput::text(content_text)
                    };

                    return Some(Message::tool_result(&tool_use_id, &output));
                }
            }
        }
    }
    None
}

/// Extract title from first user message.
fn extract_title(messages: &[Message]) -> String {
    for msg in messages {
        if msg.role == Role::User {
            let text = msg.text();
            let title: String = text.chars().take(50).collect();
            if title.len() < text.len() {
                return format!("{}...", title);
            }
            return title;
        }
    }
    "Imported session".to_string()
}

/// List Claude Code sessions for a project.
pub fn list_claude_sessions(project_path: &Path) -> Result<Vec<(std::path::PathBuf, SessionMeta)>> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    let claude_projects = home.join(".claude").join("projects");

    if !claude_projects.exists() {
        return Ok(Vec::new());
    }

    // Convert project path to Claude Code directory name
    let project_str = project_path.to_string_lossy().to_string();
    let sanitized = project_str
        .replace(':', "")
        .replace('\\', "-")
        .replace('/', "-");

    let mut sessions = Vec::new();

    // Check all project directories
    for entry in std::fs::read_dir(&claude_projects)? {
        let entry = entry?;
        let dir_name = entry.file_name().to_string_lossy().to_string();

        // Check if this directory matches our project
        if dir_name == sanitized || dir_name.starts_with(&sanitized) {
            let dir_path = entry.path();

            // Try sessions-index.json first (faster)
            let index_path = dir_path.join("sessions-index.json");
            if index_path.exists() {
                if let Ok(sessions_from_index) = list_sessions_from_index(&index_path, &dir_path) {
                    sessions.extend(sessions_from_index);
                }
            }

            // Fallback to scanning JSONL files
            if sessions.is_empty() {
                for file_entry in std::fs::read_dir(&dir_path)? {
                    let file_entry = file_entry?;
                    let file_path = file_entry.path();

                    if file_path.extension().map_or(false, |e| e == "jsonl") {
                        if let Ok((_, meta)) = import_claude_code(&file_path) {
                            sessions.push((file_path, meta));
                        }
                    }
                }
            }
        }
    }

    // Also scan all project directories for JSONL files (like jcode does)
    for entry in std::fs::read_dir(&claude_projects)? {
        let entry = entry?;
        let dir_path = entry.path();
        if !dir_path.is_dir() {
            continue;
        }

        for file_entry in std::fs::read_dir(&dir_path)? {
            let file_entry = file_entry?;
            let file_path = file_entry.path();

            if file_path.extension().map_or(false, |e| e == "jsonl") {
                // Skip if already found
                if sessions.iter().any(|(p, _)| p == &file_path) {
                    continue;
                }

                if let Ok((_, meta)) = import_claude_code(&file_path) {
                    sessions.push((file_path, meta));
                }
            }
        }
    }

    // Sort by updated_at descending
    sessions.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));

    Ok(sessions)
}

/// List sessions from Claude Code sessions-index.json
fn list_sessions_from_index(index_path: &Path, project_dir: &Path) -> Result<Vec<(std::path::PathBuf, SessionMeta)>> {
    let content = std::fs::read_to_string(index_path)?;
    let index: serde_json::Value = serde_json::from_str(&content)?;

    let mut sessions = Vec::new();

    if let Some(entries) = index.get("entries").and_then(|e| e.as_array()) {
        for entry in entries {
            // Skip sidechain sessions
            if entry.get("is_sidechain").and_then(|v| v.as_bool()).unwrap_or(false) {
                continue;
            }

            let session_id = entry.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
            let jsonl_path = project_dir.join(format!("{}.jsonl", session_id));

            if !jsonl_path.exists() {
                continue;
            }

            let first_prompt = entry.get("first_prompt").and_then(|v| v.as_str()).unwrap_or("");
            let summary = entry.get("summary").and_then(|v| v.as_str());
            let created = entry.get("created").and_then(|v| v.as_str()).unwrap_or("");
            let modified = entry.get("modified").and_then(|v| v.as_str()).unwrap_or("");
            let message_count = entry.get("message_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            let title = if !first_prompt.is_empty() {
                first_prompt.chars().take(50).collect::<String>()
            } else if let Some(s) = summary {
                s.chars().take(50).collect::<String>()
            } else {
                format!("Session {}", &session_id[..8.min(session_id.len())])
            };

            let meta = SessionMeta {
                id: session_id.to_string(),
                created_at: created.to_string(),
                updated_at: modified.to_string(),
                provider: "claude-code".to_string(),
                model: String::new(), // Will be filled on import
                title,
                message_count,
            };

            sessions.push((jsonl_path, meta));
        }
    }

    Ok(sessions)
}
