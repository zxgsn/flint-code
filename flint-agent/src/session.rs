//! Conversation session management.

use flint_types::{ContentBlock, Message, ToolCall, ToolOutput};

/// A conversation session holding message history.
pub struct Session {
    pub messages: Vec<Message>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
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
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
