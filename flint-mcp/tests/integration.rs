//! Integration tests for MCP client against a real server.

use flint_agent::{Tool, ToolContext};
use flint_mcp::{McpClient, McpManager};
use std::collections::HashMap;
use std::path::PathBuf;

/// Path to the test MCP server script.
fn test_server_path() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest_dir)
        .parent()
        .unwrap()
        .join("test-mcp-server.js");
    path.to_string_lossy().to_string()
}

#[tokio::test]
async fn test_spawn_and_handshake() {
    let server = test_server_path();
    let args = vec![server];
    let env = HashMap::new();

    let (client, init) = McpClient::spawn("node", &args, &env).await.unwrap();

    assert_eq!(init.protocol_version, "2024-11-05");
    assert_eq!(init.server_info.name, "test-echo-server");
    assert_eq!(init.server_info.version, "0.1.0");
    assert!(init.capabilities.tools.is_some());

    client.shutdown().await;
}

#[tokio::test]
async fn test_list_tools() {
    let server = test_server_path();
    let args = vec![server];
    let env = HashMap::new();

    let (client, _) = McpClient::spawn("node", &args, &env).await.unwrap();

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 2);

    let echo = tools.iter().find(|t| t.name == "echo").unwrap();
    assert_eq!(echo.description, "Echo back the input message");
    assert!(echo.input_schema.is_object());

    let add = tools.iter().find(|t| t.name == "add").unwrap();
    assert_eq!(add.description, "Add two numbers");

    client.shutdown().await;
}

#[tokio::test]
async fn test_call_echo_tool() {
    let server = test_server_path();
    let args = vec![server];
    let env = HashMap::new();

    let (client, _) = McpClient::spawn("node", &args, &env).await.unwrap();

    let result = client
        .call_tool("echo", serde_json::json!({"message": "hello flint"}))
        .await
        .unwrap();

    assert!(!result.is_error);
    assert_eq!(result.content.len(), 1);

    match &result.content[0] {
        flint_mcp::protocol::ContentBlock::Text { text } => {
            assert_eq!(text, "Echo: hello flint");
        }
        _ => panic!("expected text content block"),
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_call_add_tool() {
    let server = test_server_path();
    let args = vec![server];
    let env = HashMap::new();

    let (client, _) = McpClient::spawn("node", &args, &env).await.unwrap();

    let result = client
        .call_tool("add", serde_json::json!({"a": 3, "b": 7}))
        .await
        .unwrap();

    assert!(!result.is_error);

    match &result.content[0] {
        flint_mcp::protocol::ContentBlock::Text { text } => {
            assert_eq!(text, "3 + 7 = 10");
        }
        _ => panic!("expected text content block"),
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_call_unknown_tool() {
    let server = test_server_path();
    let args = vec![server];
    let env = HashMap::new();

    let (client, _) = McpClient::spawn("node", &args, &env).await.unwrap();

    let result = client
        .call_tool("nonexistent", serde_json::json!({}))
        .await
        .unwrap();

    assert!(result.is_error);

    client.shutdown().await;
}

#[tokio::test]
async fn test_mcp_tool_trait_adapter() {
    let server = test_server_path();
    let args = vec![server];
    let env = HashMap::new();

    let (client, _) = McpClient::spawn("node", &args, &env).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    let echo_info = tools.into_iter().find(|t| t.name == "echo").unwrap();

    let mcp_tool = flint_mcp::McpTool {
        server_id: "test".to_string(),
        info: echo_info,
        client: std::sync::Arc::new(client),
    };

    // Check definition
    let def = mcp_tool.definition();
    assert_eq!(def.name, "mcp__test__echo");
    assert!(def.description.contains("[MCP:test]"));
    assert!(def.description.contains("Echo back"));

    // Execute via Tool trait
    let ctx = ToolContext {
        working_dir: PathBuf::from("."),
    };
    let output = mcp_tool
        .execute(serde_json::json!({"message": "via trait"}), &ctx)
        .await
        .unwrap();

    assert!(!output.is_error);
    assert_eq!(output.text, "Echo: via trait");
}

#[tokio::test]
async fn test_mcp_manager_connect_and_status() {
    let server = test_server_path();
    let mut configs = HashMap::new();
    configs.insert(
        "test".to_string(),
        flint_mcp::McpServerConfig {
            command: "node".to_string(),
            args: vec![server],
            env: HashMap::new(),
        },
    );

    let mut manager = McpManager::new();
    let tools = manager.connect_all(&configs).await.unwrap();

    assert_eq!(tools.len(), 2); // echo + add

    let status = manager.status();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].0, "test");
    assert_eq!(status[0].1, 2);

    manager.shutdown().await;
}

#[tokio::test]
async fn test_mcp_manager_bad_server() {
    let mut configs = HashMap::new();
    configs.insert(
        "bad".to_string(),
        flint_mcp::McpServerConfig {
            command: "nonexistent_command_xyz".to_string(),
            args: vec![],
            env: HashMap::new(),
        },
    );

    let mut manager = McpManager::new();
    let tools = manager.connect_all(&configs).await.unwrap();

    // Should return empty tools (server failed to connect)
    assert!(tools.is_empty());
    assert!(manager.status().is_empty());

    manager.shutdown().await;
}
