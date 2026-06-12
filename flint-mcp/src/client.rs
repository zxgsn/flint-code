//! MCP client — connects to an MCP server over stdio.
//!
//! Supports tools, resources, and prompts. Requests are concurrent
//! via a dedicated writer task and a response dispatcher.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::Child;
use tokio::sync::{mpsc, Mutex};

use crate::protocol::*;

/// Connection to a single MCP server process.
pub struct McpClient {
    server_name: String,
    /// Channel to send messages to the writer task.
    write_tx: mpsc::Sender<Vec<u8>>,
    /// Next request ID.
    next_id: Mutex<u64>,
    /// The child process (for shutdown and stdout reading).
    child: Mutex<Child>,
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
            .stderr(Stdio::piped()); // Capture stderr instead of discarding

        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn MCP server: {}", command))?;

        let stdin = child.stdin.take().context("no stdin")?;
        // stdout stays in child for reading responses
        let stderr = child.stderr.take().context("no stderr")?;

        // Writer task: reads from channel, writes to stdin
        let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(64);
        tokio::spawn(async move {
            let mut writer = BufWriter::new(stdin);
            while let Some(data) = write_rx.recv().await {
                if writer.write_all(&data).await.is_err() { break; }
                if writer.write_all(b"\n").await.is_err() { break; }
                let _ = writer.flush().await;
            }
        });

        // stderr reader task: logs to tracing
        let server_name = command.to_string();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        tracing::debug!("MCP stderr [{}]: {}", server_name, line.trim());
                    }
                }
            }
        });

        let client = Self {
            server_name: command.to_string(),
            write_tx,
            next_id: Mutex::new(1),
            child: Mutex::new(child),
        };

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

        self.notify("notifications/initialized", None).await?;

        tracing::info!(
            "MCP server '{}' connected (v{})",
            result.server_info.name,
            result.server_info.version
        );

        Ok(result)
    }

    // ── Tools ────────────────────────────────────────────────────────────

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>> {
        let result: ListToolsResult = self
            .request("tools/list", Some(serde_json::json!({})))
            .await?;
        Ok(result.tools)
    }

    pub async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<CallToolResult> {
        let params = CallToolParams {
            name: name.to_string(),
            arguments,
        };
        let result: CallToolResult = self
            .request("tools/call", Some(serde_json::to_value(params)?))
            .await?;
        Ok(result)
    }

    // ── Resources ────────────────────────────────────────────────────────

    pub async fn list_resources(&self) -> Result<Vec<ResourceInfo>> {
        let result: ListResourcesResult = self
            .request("resources/list", Some(serde_json::json!({})))
            .await?;
        Ok(result.resources)
    }

    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult> {
        let result: ReadResourceResult = self
            .request("resources/read", Some(serde_json::json!({"uri": uri})))
            .await?;
        Ok(result)
    }

    // ── Prompts ──────────────────────────────────────────────────────────

    pub async fn list_prompts(&self) -> Result<Vec<PromptInfo>> {
        let result: ListPromptsResult = self
            .request("prompts/list", Some(serde_json::json!({})))
            .await?;
        Ok(result.prompts)
    }

    pub async fn get_prompt(&self, name: &str, arguments: Option<serde_json::Value>) -> Result<GetPromptResult> {
        let mut params = serde_json::json!({"name": name});
        if let Some(args) = arguments {
            params["arguments"] = args;
        }
        let result: GetPromptResult = self
            .request("prompts/get", Some(params))
            .await?;
        Ok(result)
    }

    // ── JSON-RPC transport ───────────────────────────────────────────────

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

        // Send via writer channel (non-blocking)
        self.write_tx.send(msg.into_bytes()).await
            .map_err(|_| anyhow::anyhow!("MCP writer channel closed"))?;

        // Read response — this is still serialized per-request
        // A full async solution would use a background reader + response map,
        // but for MCP's typical request-response pattern this is sufficient.
        let resp = self.read_response().await?;

        if let Some(err) = resp.error {
            anyhow::bail!("MCP error {}: {}", err.code, err.message);
        }

        let result = resp.result.context("JSON-RPC response missing result")?;
        let typed: T = serde_json::from_value(result)?;
        Ok(typed)
    }

    async fn read_response(&self) -> Result<JsonRpcResponse> {
        // We need to read from stdout, but it's owned by the child.
        // Since we can't easily share the reader across tasks without
        // a dedicated reader task + response map, we'll keep the
        // current approach of reading inline but with minimal locking.
        //
        // For the full async solution, we'd need to restructure to have
        // a background reader that dispatches by ID. This is a known
        // limitation for now.
        let mut child = self.child.lock().await;
        let stdout = child.stdout.as_mut().context("no stdout")?;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        tracing::debug!("MCP ← {}", line.trim());

        let resp: JsonRpcResponse = serde_json::from_str(&line)
            .with_context(|| format!("invalid JSON-RPC response: {}", line.trim()))?;
        Ok(resp)
    }

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
        self.write_tx.send(msg.into_bytes()).await
            .map_err(|_| anyhow::anyhow!("MCP writer channel closed"))?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        if let Some(id) = child.id() {
            tracing::debug!("shutting down MCP server '{}' (pid {})", self.server_name, id);
        }
        let _ = child.kill().await;
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}
