//! JSON-RPC 2.0 types and MCP protocol messages.

use serde::{Deserialize, Serialize};

// ── JSON-RPC 2.0 ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    pub id: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

// ── MCP: initialize ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

#[derive(Debug, Serialize)]
pub struct ClientCapabilities {}

#[derive(Debug, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Deserialize)]
pub struct ServerCapabilities {
    pub tools: Option<ToolsCapability>,
}

#[derive(Debug, Deserialize)]
pub struct ToolsCapability {}

#[derive(Debug, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

// ── MCP: tools/list ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<ToolInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

// ── MCP: tools/call ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CallToolParams {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct CallToolResult {
    pub content: Vec<ContentBlock>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, #[serde(rename = "mimeType")] mime_type: String },
    #[serde(rename = "resource")]
    Resource { resource: serde_json::Value },
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "initialize".to_string(),
            params: Some(serde_json::json!({"protocolVersion": "2024-11-05"})),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"method\":\"initialize\""));
        assert!(s.contains("\"id\":1"));
    }

    #[test]
    fn test_request_no_params() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 2,
            method: "tools/list".to_string(),
            params: None,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(!s.contains("params"));
    }

    #[test]
    fn test_response_with_result() {
        let json = r#"{"id": 1, "result": {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "test", "version": "1.0"}}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(1));
        assert!(resp.error.is_none());
        let init: InitializeResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert_eq!(init.server_info.name, "test");
    }

    #[test]
    fn test_response_with_error() {
        let json = r#"{"id": 1, "error": {"code": -32600, "message":: "Invalid Request"}}"#;
        // This should fail to parse because of the double colon typo
        let resp: Result<JsonRpcResponse, _> = serde_json::from_str(json);
        assert!(resp.is_err());
    }

    #[test]
    fn test_error_response() {
        let json = r#"{"id": 1, "error": {"code": -32600, "message": "Invalid Request"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32600);
        assert_eq!(err.message, "Invalid Request");
    }

    #[test]
    fn test_list_tools_result() {
        let json = r#"{"tools": [
            {"name": "read_file", "description": "Read a file", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}},
            {"name": "write_file", "inputSchema": {"type": "object"}}
        ]}"#;
        let result: ListToolsResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.tools.len(), 2);
        assert_eq!(result.tools[0].name, "read_file");
        assert_eq!(result.tools[0].description, "Read a file");
        assert_eq!(result.tools[1].description, ""); // default
    }

    #[test]
    fn test_call_tool_result_text() {
        let json = r#"{"content": [{"type": "text", "text": "hello world"}], "isError": false}"#;
        let result: CallToolResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.content.len(), 1);
        assert!(!result.is_error);
        match &result.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello world"),
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn test_call_tool_result_error() {
        let json = r#"{"content": [{"type": "text", "text": "not found"}], "isError": true}"#;
        let result: CallToolResult = serde_json::from_str(json).unwrap();
        assert!(result.is_error);
    }

    #[test]
    fn test_call_tool_result_no_is_error() {
        // isError is optional, should default to false
        let json = r#"{"content": [{"type": "text", "text": "ok"}]}"#;
        let result: CallToolResult = serde_json::from_str(json).unwrap();
        assert!(!result.is_error);
    }

    #[test]
    fn test_call_tool_params_serialization() {
        let params = CallToolParams {
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "/tmp/test.txt"}),
        };
        let s = serde_json::to_string(&params).unwrap();
        assert!(s.contains("\"name\":\"read_file\""));
        assert!(s.contains("\"path\":\"/tmp/test.txt\""));
    }

    #[test]
    fn test_tool_info_input_schema() {
        let json = r#"{
            "name": "search",
            "description": "Search files",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "limit": {"type": "integer", "default": 10}
                },
                "required": ["query"]
            }
        }"#;
        let info: ToolInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.name, "search");
        assert!(info.input_schema.is_object());
        let props = &info.input_schema["properties"];
        assert!(props["query"].is_object());
    }
}
