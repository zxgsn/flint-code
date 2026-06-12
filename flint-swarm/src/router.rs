//! Cross-platform TCP-based message router for swarm agents.
//!
//! The MessageRouter runs in the coordinator process and accepts TCP connections
//! from sub-agents. It routes messages between agents in real-time.
//!
//! Protocol: newline-delimited JSON over TCP on 127.0.0.1.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};

/// Messages exchanged between the router and agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RouterMessage {
    /// Agent registers itself with the router.
    Register { agent_id: String },
    /// Send a message to a specific agent.
    Send { from: String, to: String, content: String },
    /// Broadcast a message to all agents.
    Broadcast { from: String, content: String },
    /// Agent reports its task result.
    Result { agent_id: String, task_id: String, result: String },
    /// Request list of connected agents.
    ListAgents,
    /// Response to ListAgents.
    AgentList { agents: Vec<String> },
    /// Stop an agent.
    Stop { agent_id: String },
    /// Acknowledgment.
    Ack { ok: bool, message: String },
    /// Incoming message for an agent (sent by router to agent).
    Incoming { from: String, content: String },
    /// Structured notification from a sub-agent to the coordinator.
    Notify { agent_id: String, kind: String, content: String },
    /// Error message.
    Error { message: String },
}

/// A connected agent.
struct ConnectedAgent {
    writer: Arc<Mutex<tokio::io::WriteHalf<TcpStream>>>,
}

/// The message router — accepts connections and routes messages.
pub struct MessageRouter {
    /// TCP listener address (127.0.0.1:port).
    pub addr: std::net::SocketAddr,
    /// Connected agents: agent_id → writer half.
    agents: Arc<Mutex<HashMap<String, ConnectedAgent>>>,
    /// Channel for result messages from agents.
    result_tx: mpsc::Sender<AgentResult>,
    result_rx: Arc<Mutex<mpsc::Receiver<AgentResult>>>,
    /// Channel for incoming messages to agents (coordinator → agent).
    inbound_tx: mpsc::Sender<InboundMessage>,
    inbound_rx: Arc<Mutex<mpsc::Receiver<InboundMessage>>>,
}

/// Result reported by an agent through the router.
#[derive(Debug, Clone)]
pub struct AgentResult {
    pub agent_id: String,
    pub task_id: String,
    pub result: String,
}

/// Message to be delivered to an agent.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub from: String,
    pub to: String,
    pub content: String,
}

impl MessageRouter {
    /// Create and start the message router.
    /// Returns the router and the address it's listening on.
    pub async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let agents: Arc<Mutex<HashMap<String, ConnectedAgent>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (result_tx, result_rx) = mpsc::channel(64);
        let (inbound_tx, inbound_rx) = mpsc::channel(64);

        let router = Self {
            addr,
            agents: agents.clone(),
            result_tx: result_tx.clone(),
            result_rx: Arc::new(Mutex::new(result_rx)),
            inbound_tx: inbound_tx.clone(),
            inbound_rx: Arc::new(Mutex::new(inbound_rx)),
        };

        // Spawn the accept loop
        let agents_clone = agents.clone();
        let result_tx_clone = result_tx.clone();
        let inbound_tx_clone = inbound_tx.clone();
        tokio::spawn(async move {
            Self::accept_loop(listener, agents_clone, result_tx_clone, inbound_tx_clone).await;
        });

        Ok(router)
    }

    /// The accept loop — listens for new connections and spawns handler tasks.
    async fn accept_loop(
        listener: TcpListener,
        agents: Arc<Mutex<HashMap<String, ConnectedAgent>>>,
        result_tx: mpsc::Sender<AgentResult>,
        inbound_tx: mpsc::Sender<InboundMessage>,
    ) {
        loop {
            match listener.accept().await {
                Ok((stream, _peer_addr)) => {
                    let agents = agents.clone();
                    let result_tx = result_tx.clone();
                    let inbound_tx = inbound_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_connection(stream, agents, result_tx, inbound_tx).await {
                            eprintln!("[router] connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[router] accept error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Handle a single agent connection.
    async fn handle_connection(
        stream: TcpStream,
        agents: Arc<Mutex<HashMap<String, ConnectedAgent>>>,
        result_tx: mpsc::Sender<AgentResult>,
        _inbound_tx: mpsc::Sender<InboundMessage>,
    ) -> Result<()> {
        let (read_half, write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let writer = Arc::new(Mutex::new(write_half));
        let mut agent_id: Option<String> = None;

        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break; // Connection closed
            }

            let msg: RouterMessage = match serde_json::from_str(line.trim()) {
                Ok(m) => m,
                Err(e) => {
                    let err = RouterMessage::Error {
                        message: format!("invalid message: {}", e),
                    };
                    let mut w = writer.lock().await;
                    let _ = w.write_all(serde_json::to_string(&err)?.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                    continue;
                }
            };

            match msg {
                RouterMessage::Register { agent_id: ref reg_id } => {
                    agent_id = Some(reg_id.clone());
                    agents.lock().await.insert(reg_id.clone(), ConnectedAgent {
                        writer: writer.clone(),
                    });
                    let ack = RouterMessage::Ack {
                        ok: true,
                        message: format!("registered as {}", reg_id),
                    };
                    let mut w = writer.lock().await;
                    let _ = w.write_all(serde_json::to_string(&ack)?.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                RouterMessage::Send { from, to, content } => {
                    let agents_map = agents.lock().await;
                    if let Some(target) = agents_map.get(&to) {
                        let incoming = RouterMessage::Incoming {
                            from: from.clone(),
                            content: content.clone(),
                        };
                        let mut w = target.writer.lock().await;
                        let _ = w.write_all(serde_json::to_string(&incoming)?.as_bytes()).await;
                        let _ = w.write_all(b"\n").await;
                        // Ack to sender
                        let ack = RouterMessage::Ack {
                            ok: true,
                            message: format!("sent to {}", to),
                        };
                        let mut sender_w = writer.lock().await;
                        let _ = sender_w.write_all(serde_json::to_string(&ack)?.as_bytes()).await;
                        let _ = sender_w.write_all(b"\n").await;
                    } else {
                        let err = RouterMessage::Error {
                            message: format!("agent '{}' not found", to),
                        };
                        let mut w = writer.lock().await;
                        let _ = w.write_all(serde_json::to_string(&err)?.as_bytes()).await;
                        let _ = w.write_all(b"\n").await;
                    }
                }
                RouterMessage::Broadcast { from, content } => {
                    let agents_map = agents.lock().await;
                    for (id, agent) in agents_map.iter() {
                        if id != &from {
                            let incoming = RouterMessage::Incoming {
                                from: from.clone(),
                                content: content.clone(),
                            };
                            let mut w = agent.writer.lock().await;
                            let _ = w.write_all(serde_json::to_string(&incoming)?.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                        }
                    }
                    let ack = RouterMessage::Ack {
                        ok: true,
                        message: "broadcast sent".to_string(),
                    };
                    let mut w = writer.lock().await;
                    let _ = w.write_all(serde_json::to_string(&ack)?.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                RouterMessage::Result { agent_id: aid, task_id, result } => {
                    let _ = result_tx.send(AgentResult {
                        agent_id: aid.clone(),
                        task_id: task_id.clone(),
                        result: result.clone(),
                    }).await;
                    let ack = RouterMessage::Ack {
                        ok: true,
                        message: "result received".to_string(),
                    };
                    let mut w = writer.lock().await;
                    let _ = w.write_all(serde_json::to_string(&ack)?.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                RouterMessage::ListAgents => {
                    let agents_map = agents.lock().await;
                    let agent_list: Vec<String> = agents_map.keys().cloned().collect();
                    let resp = RouterMessage::AgentList { agents: agent_list };
                    let mut w = writer.lock().await;
                    let _ = w.write_all(serde_json::to_string(&resp)?.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                RouterMessage::Stop { agent_id: target_id } => {
                    let agents_map = agents.lock().await;
                    if let Some(target) = agents_map.get(&target_id) {
                        let stop = RouterMessage::Stop {
                            agent_id: target_id.clone(),
                        };
                        let mut w = target.writer.lock().await;
                        let _ = w.write_all(serde_json::to_string(&stop)?.as_bytes()).await;
                        let _ = w.write_all(b"\n").await;
                    }
                    let ack = RouterMessage::Ack {
                        ok: true,
                        message: format!("stop sent to {}", target_id),
                    };
                    let mut w = writer.lock().await;
                    let _ = w.write_all(serde_json::to_string(&ack)?.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                RouterMessage::Notify { agent_id: ref from_id, ref kind, ref content } => {
                    // Forward notification to coordinator as an Incoming message
                    // with a [NOTIFY:kind] prefix for easy parsing.
                    let agents_map = agents.lock().await;
                    // Find the coordinator (the first non-sub-agent connection, or broadcast to all)
                    for (id, agent) in agents_map.iter() {
                        if id != from_id {
                            let incoming = RouterMessage::Incoming {
                                from: from_id.clone(),
                                content: format!("[NOTIFY:{}]: {}", kind, content),
                            };
                            let mut w = agent.writer.lock().await;
                            let _ = w.write_all(serde_json::to_string(&incoming)?.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                        }
                    }
                    let ack = RouterMessage::Ack {
                        ok: true,
                        message: "notification sent".to_string(),
                    };
                    let mut w = writer.lock().await;
                    let _ = w.write_all(serde_json::to_string(&ack)?.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                _ => {
                    // Ignore other messages
                }
            }
        }

        // Connection closed — remove agent
        if let Some(id) = agent_id {
            agents.lock().await.remove(&id);
        }

        Ok(())
    }

    /// Send a message from the coordinator to an agent through the router.
    pub async fn send_to_agent(&self, to: &str, content: &str) -> Result<()> {
        let agents = self.agents.lock().await;
        if let Some(target) = agents.get(to) {
            let incoming = RouterMessage::Incoming {
                from: "coordinator".to_string(),
                content: content.to_string(),
            };
            let mut w = target.writer.lock().await;
            w.write_all(serde_json::to_string(&incoming)?.as_bytes()).await?;
            w.write_all(b"\n").await?;
            Ok(())
        } else {
            Err(anyhow::anyhow!("agent '{}' not connected to router", to))
        }
    }

    /// Drain all pending results from agents.
    pub async fn drain_results(&self) -> Vec<AgentResult> {
        let mut rx = self.result_rx.lock().await;
        let mut results = Vec::new();
        while let Ok(result) = rx.try_recv() {
            results.push(result);
        }
        results
    }

    /// Get the number of connected agents.
    pub async fn agent_count(&self) -> usize {
        self.agents.lock().await.len()
    }

    /// List connected agent IDs.
    pub async fn list_agents(&self) -> Vec<String> {
        self.agents.lock().await.keys().cloned().collect()
    }

    /// Check if an agent is connected.
    pub async fn is_connected(&self, agent_id: &str) -> bool {
        self.agents.lock().await.contains_key(agent_id)
    }
}
