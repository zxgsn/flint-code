//! Agent endpoint — TCP client for sub-agents to connect to the MessageRouter.
//!
//! Used by interactive sub-agents (separate process) to receive messages
//! from the coordinator in real-time.

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::router::RouterMessage;

/// Client-side connection to the MessageRouter.
///
/// An AgentEndpoint connects to the router on startup, registers itself,
/// and can then send/receive messages in real-time.
pub struct AgentEndpoint {
    pub agent_id: String,
    reader: BufReader<tokio::io::ReadHalf<TcpStream>>,
    writer: tokio::io::WriteHalf<TcpStream>,
}

impl AgentEndpoint {
    /// Connect to the MessageRouter and register this agent.
    pub async fn connect(addr: &str, agent_id: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        let (read_half, write_half) = tokio::io::split(stream);
        let mut endpoint = Self {
            agent_id: agent_id.to_string(),
            reader: BufReader::new(read_half),
            writer: write_half,
        };

        // Register with the router
        let register = RouterMessage::Register {
            agent_id: agent_id.to_string(),
        };
        endpoint.send_message(&register).await?;

        // Wait for acknowledgment
        let ack = endpoint.read_message().await?;
        match ack {
            RouterMessage::Ack { ok: true, .. } => Ok(endpoint),
            RouterMessage::Error { message } => {
                Err(anyhow::anyhow!("registration failed: {}", message))
            }
            other => {
                Err(anyhow::anyhow!("unexpected response: {:?}", other))
            }
        }
    }

    /// Send a message to the router.
    pub async fn send_message(&mut self, msg: &RouterMessage) -> Result<()> {
        let json = serde_json::to_string(msg)?;
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        Ok(())
    }

    /// Read the next message from the router (blocking).
    pub async fn read_message(&mut self) -> Result<RouterMessage> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(anyhow::anyhow!("connection closed"));
        }
        let msg: RouterMessage = serde_json::from_str(line.trim())?;
        Ok(msg)
    }

    /// Try to read a message with a short timeout (non-blocking feel).
    /// Returns None if no message is available within the timeout.
    pub async fn try_read_message(&mut self) -> Result<Option<RouterMessage>> {
        let mut line = String::new();
        match tokio::time::timeout(
            std::time::Duration::from_millis(50),
            self.reader.read_line(&mut line),
        ).await {
            Ok(Ok(0)) => Err(anyhow::anyhow!("connection closed")),
            Ok(Ok(_)) => {
                let msg: RouterMessage = serde_json::from_str(line.trim())?;
                Ok(Some(msg))
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("read error: {}", e)),
            Err(_) => Ok(None), // Timeout — no data available
        }
    }

    /// Report a task result to the router.
    pub async fn report_result(&mut self, task_id: &str, result: &str) -> Result<()> {
        let msg = RouterMessage::Result {
            agent_id: self.agent_id.clone(),
            task_id: task_id.to_string(),
            result: result.to_string(),
        };
        self.send_message(&msg).await
    }

    /// Send a message to another agent through the router.
    pub async fn send_to(&mut self, target: &str, content: &str) -> Result<()> {
        let msg = RouterMessage::Send {
            from: self.agent_id.clone(),
            to: target.to_string(),
            content: content.to_string(),
        };
        self.send_message(&msg).await
    }

    /// Broadcast a message to all agents.
    pub async fn broadcast(&mut self, content: &str) -> Result<()> {
        let msg = RouterMessage::Broadcast {
            from: self.agent_id.clone(),
            content: content.to_string(),
        };
        self.send_message(&msg).await
    }

    /// Send a structured notification to the coordinator.
    /// `kind` is one of: "progress", "question", "context_request".
    pub async fn send_notify(&mut self, kind: &str, content: &str) -> Result<()> {
        let msg = RouterMessage::Notify {
            agent_id: self.agent_id.clone(),
            kind: kind.to_string(),
            content: content.to_string(),
        };
        self.send_message(&msg).await
    }

    /// Request list of connected agents.
    pub async fn list_agents(&mut self) -> Result<Vec<String>> {
        let msg = RouterMessage::ListAgents;
        self.send_message(&msg).await?;
        let resp = self.read_message().await?;
        match resp {
            RouterMessage::AgentList { agents } => Ok(agents),
            _ => Err(anyhow::anyhow!("unexpected response: {:?}", resp)),
        }
    }
}
