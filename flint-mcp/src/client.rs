//! MCP client — connects to a single MCP server over stdio.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::protocol::*;

/// Connection to a single MCP server process.
pub struct McpClient {
    server_name: String,
    stdin: Mutex<BufWriter<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
    child: Mutex<Child>,
    next_id: Mutex<u64>,
}

impl McpClient {
    /// Spawn an MCP server process and perform the handshake.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<(Self, InitializeResult)> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn MCP server: {}", command))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;

        let client = Self {
            server_name: command.to_string(),
            stdin: Mutex::new(BufWriter::new(stdin)),
            stdout: Mutex::new(BufReader::new(stdout)),
            child: Mutex::new(child),
            next_id: Mutex::new(1),
        };

        // Perform handshake: initialize → notifications/initialized
        let init_result = client.initialize().await?;

        Ok((client, init_result))
    }

    /// Perform the MCP initialize handshake.
    async fn initialize(&self) -> Result<InitializeResult> {
        let params = InitializeParams {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ClientCapabilities {},
            client_info: ClientInfo {
                name: "flint".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let result: InitializeResult = self
            .request("initialize", Some(serde_json::to_value(params)?))
            .await?;

        // Send initialized notification (no response expected)
        self.notify("notifications/initialized", None).await?;

        tracing::info!(
            "MCP server '{}' connected (v{})",
            result.server_info.name,
            result.server_info.version
        );

        Ok(result)
    }

    /// List available tools from the server.
    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>> {
        let result: ListToolsResult = self
            .request("tools/list", Some(serde_json::json!({})))
            .await?;
        Ok(result.tools)
    }

    /// Call a tool on the server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult> {
        let params = CallToolParams {
            name: name.to_string(),
            arguments,
        };
        let result: CallToolResult = self
            .request("tools/call", Some(serde_json::to_value(params)?))
            .await?;
        Ok(result)
    }

    /// Send a JSON-RPC request and wait for the response.
    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<T> {
        let id = {
            let mut id = self.next_id.lock().await;
            let current = *id;
            *id += 1;
            current
        };

        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        let msg = serde_json::to_string(&req)?;
        tracing::debug!("MCP → {}", msg);

        // Write request
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(msg.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }

        // Read response — lock stdout to ensure we get the right response
        let mut stdout = self.stdout.lock().await;
        let mut line = String::new();
        stdout.read_line(&mut line).await?;
        tracing::debug!("MCP ← {}", line.trim());

        let resp: JsonRpcResponse = serde_json::from_str(&line)
            .with_context(|| format!("invalid JSON-RPC response: {}", line.trim()))?;

        if let Some(err) = resp.error {
            anyhow::bail!("MCP error {}: {}", err.code, err.message);
        }

        let result = resp.result.context("JSON-RPC response missing result")?;
        let typed: T = serde_json::from_value(result)?;
        Ok(typed)
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let msg = if let Some(p) = params {
            serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": p,
            }))?
        } else {
            serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": method,
            }))?
        };

        tracing::debug!("MCP → {}", msg);

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(msg.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Shut down the server process.
    pub async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        if let Some(id) = child.id() {
            tracing::debug!("shutting down MCP server '{}' (pid {})", self.server_name, id);
        }
        let _ = child.kill().await;
    }

    /// Get the server name (command used to spawn it).
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}
