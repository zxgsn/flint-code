//! MCP client over HTTP/SSE transport.
//!
//! Connects to an MCP server via HTTP. Uses SSE for server-to-client
//! events and HTTP POST for client-to-server requests.
//!
//! Transport flow:
//! 1. GET /sse → opens SSE stream, receives `endpoint` event with POST URL
//! 2. POST <endpoint> → sends JSON-RPC request
//! 3. SSE stream delivers responses as `message` events

use anyhow::{Context, Result};
use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};

use crate::protocol::*;

/// Connection to an MCP server over HTTP/SSE.
pub struct HttpMcpClient {
    server_name: String,
    base_url: String,
    http_client: reqwest::Client,
    /// Endpoint URL received from SSE `endpoint` event.
    endpoint: Mutex<Option<String>>,
    /// Channel for receiving SSE messages.
    message_rx: Mutex<mpsc::Receiver<String>>,
    /// Next request ID.
    next_id: Mutex<u64>,
}

impl HttpMcpClient {
    /// Connect to an MCP server via HTTP/SSE.
    pub async fn connect(url: &str) -> Result<(Self, InitializeResult)> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()?;

        // Start SSE connection
        let resp = http_client
            .get(url)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .with_context(|| format!("failed to connect to MCP SSE endpoint: {}", url))?;

        if !resp.status().is_success() {
            anyhow::bail!("MCP SSE endpoint returned HTTP {}", resp.status());
        }

        let (message_tx, message_rx) = mpsc::channel::<String>(256);

        // Spawn SSE reader task
        tokio::spawn(async move {
            let mut stream = resp.bytes_stream();
            let mut buffer = String::new();
            let mut current_event = String::new();

            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let text = String::from_utf8_lossy(&chunk);
                buffer.push_str(&text);

                // Parse SSE events
                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                    buffer.drain(..=newline_pos);

                    if line.is_empty() {
                        // Empty line = end of event
                        if !current_event.is_empty() {
                            let _ = message_tx.send(current_event.clone()).await;
                            current_event.clear();
                        }
                    } else if let Some(event_type) = line.strip_prefix("event: ") {
                        current_event = format!("event:{}", event_type);
                    } else if let Some(data) = line.strip_prefix("data: ") {
                        if current_event.starts_with("event:") {
                            current_event = format!("{}|{}", current_event, data);
                        } else {
                            current_event = data.to_string();
                        }
                    }
                }
            }
        });

        let client = Self {
            server_name: url.to_string(),
            base_url: url.to_string(),
            http_client,
            endpoint: Mutex::new(None),
            message_rx: Mutex::new(message_rx),
            next_id: Mutex::new(1),
        };

        // Wait for the endpoint event from SSE
        let endpoint = client.wait_for_endpoint().await?;

        // Perform handshake
        let init_result = client.initialize(&endpoint).await?;

        Ok((client, init_result))
    }

    /// Wait for the SSE `endpoint` event that tells us where to POST requests.
    async fn wait_for_endpoint(&self) -> Result<String> {
        let mut rx = self.message_rx.lock().await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);

        loop {
            match tokio::time::timeout(deadline - tokio::time::Instant::now(), rx.recv()).await {
                Ok(Some(msg)) => {
                    // Check if this is an endpoint event
                    if msg.starts_with("event:endpoint|") {
                        let endpoint = msg.strip_prefix("event:endpoint|").unwrap_or("");
                        if !endpoint.is_empty() {
                            // Resolve relative URL
                            let full_url = if endpoint.starts_with("http") {
                                endpoint.to_string()
                            } else {
                                // Relative URL — resolve against base
                                let base = self.base_url.trim_end_matches('/');
                                format!("{}{}", base, endpoint)
                            };
                            return Ok(full_url);
                        }
                    }
                }
                Ok(None) => anyhow::bail!("SSE stream closed before receiving endpoint"),
                Err(_) => anyhow::bail!("timeout waiting for SSE endpoint event"),
            }
        }
    }

    /// Perform the MCP initialize handshake.
    async fn initialize(&self, endpoint: &str) -> Result<InitializeResult> {
        let params = InitializeParams {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ClientCapabilities {},
            client_info: ClientInfo {
                name: "flint".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let result: InitializeResult = self
            .post_request(endpoint, "initialize", Some(serde_json::to_value(params)?))
            .await?;

        // Send initialized notification
        self.post_notify(endpoint, "notifications/initialized", None).await?;

        tracing::info!("MCP HTTP server connected (v{})", result.server_info.version);

        Ok(result)
    }

    // ── Tools ────────────────────────────────────────────────────────────

    pub async fn list_tools(&self, endpoint: &str) -> Result<Vec<ToolInfo>> {
        let result: ListToolsResult = self
            .post_request(endpoint, "tools/list", Some(serde_json::json!({})))
            .await?;
        Ok(result.tools)
    }

    pub async fn call_tool(&self, endpoint: &str, name: &str, arguments: serde_json::Value) -> Result<CallToolResult> {
        let params = CallToolParams {
            name: name.to_string(),
            arguments,
        };
        let result: CallToolResult = self
            .post_request(endpoint, "tools/call", Some(serde_json::to_value(params)?))
            .await?;
        Ok(result)
    }

    // ── Resources ────────────────────────────────────────────────────────

    pub async fn list_resources(&self, endpoint: &str) -> Result<Vec<ResourceInfo>> {
        let result: ListResourcesResult = self
            .post_request(endpoint, "resources/list", Some(serde_json::json!({})))
            .await?;
        Ok(result.resources)
    }

    pub async fn read_resource(&self, endpoint: &str, uri: &str) -> Result<ReadResourceResult> {
        let result: ReadResourceResult = self
            .post_request(endpoint, "resources/read", Some(serde_json::json!({"uri": uri})))
            .await?;
        Ok(result)
    }

    // ── Prompts ──────────────────────────────────────────────────────────

    pub async fn list_prompts(&self, endpoint: &str) -> Result<Vec<PromptInfo>> {
        let result: ListPromptsResult = self
            .post_request(endpoint, "prompts/list", Some(serde_json::json!({})))
            .await?;
        Ok(result.prompts)
    }

    pub async fn get_prompt(&self, endpoint: &str, name: &str, arguments: Option<serde_json::Value>) -> Result<GetPromptResult> {
        let mut params = serde_json::json!({"name": name});
        if let Some(args) = arguments {
            params["arguments"] = args;
        }
        let result: GetPromptResult = self
            .post_request(endpoint, "prompts/get", Some(params))
            .await?;
        Ok(result)
    }

    // ── HTTP transport ───────────────────────────────────────────────────

    async fn post_request<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
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

        let resp = self.http_client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .json(&req)
            .send()
            .await
            .with_context(|| format!("MCP HTTP request failed: {}", method))?;

        if !resp.status().is_success() {
            anyhow::bail!("MCP HTTP error {}: {}", resp.status(), resp.text().await.unwrap_or_default());
        }

        let json: JsonRpcResponse = resp.json().await
            .context("failed to parse MCP HTTP response")?;

        if let Some(err) = json.error {
            anyhow::bail!("MCP error {}: {}", err.code, err.message);
        }

        let result = json.result.context("JSON-RPC response missing result")?;
        let typed: T = serde_json::from_value(result)?;
        Ok(typed)
    }

    async fn post_notify(&self, endpoint: &str, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let msg = if let Some(p) = params {
            serde_json::json!({"jsonrpc": "2.0", "method": method, "params": p})
        } else {
            serde_json::json!({"jsonrpc": "2.0", "method": method})
        };

        self.http_client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .json(&msg)
            .send()
            .await?;

        Ok(())
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub async fn get_endpoint(&self) -> Option<String> {
        self.endpoint.lock().await.clone()
    }
}
