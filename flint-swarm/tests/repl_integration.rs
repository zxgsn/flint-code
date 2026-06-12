//! REPL-level integration tests — simulates the actual REPL loop behavior.
//!
//! These tests verify the complete flow that happens when a user interacts
//! with the swarm through the REPL:
//! 1. User sends message → run_turn with swarm tools → LLM calls swarm
//! 2. Sub-agent processes task → notification delivered
//! 3. REPL drains notifications → displays to user
//! 4. User sends follow-up → LLM calls swarm wait/followup → result returned
//!
//! Uses mock providers but tests through the actual run_turn + tool execution path.

use async_trait::async_trait;
use flint_agent::{run_turn, Session, ToolContext, ToolRegistry};
use flint_provider::{EventStream, Provider};
use flint_swarm::output;
use flint_swarm::{SwarmConfig, SwarmManager};
use flint_types::{Message, StreamEvent, ToolCall, ToolDefinition};
use futures::stream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ── Mock Provider ───────────────────────────────────────────────────────

/// Returns pre-scripted actions in order. Each action is either text or a tool call.
struct ScriptedProvider {
    actions: Mutex<Vec<Action>>,
}

enum Action {
    Text(String),
    Tool(ToolCall),
}

impl ScriptedProvider {
    fn new(actions: Vec<Action>) -> Self {
        Self { actions: Mutex::new(actions) }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str) -> anyhow::Result<EventStream> {
        let mut q = self.actions.lock().unwrap();
        if q.is_empty() {
            return Ok(Box::pin(stream::iter(vec![
                Ok(StreamEvent::TextDelta("(no more scripted responses)".into())),
                Ok(StreamEvent::End),
            ])));
        }
        match q.remove(0) {
            Action::Text(t) => Ok(Box::pin(stream::iter(vec![
                Ok(StreamEvent::TextDelta(t)),
                Ok(StreamEvent::End),
            ]))),
            Action::Tool(tc) => Ok(Box::pin(stream::iter(vec![
                Ok(StreamEvent::ToolCall(tc)),
                Ok(StreamEvent::End),
            ]))),
        }
    }
}

fn config() -> SwarmConfig {
    SwarmConfig { max_agents: 5, agent_max_turns: 10, max_output_chars: 65536, open_viewer: false }
}

fn ctx() -> ToolContext {
    ToolContext { working_dir: PathBuf::from(".") }
}

/// Set up the full REPL environment: SwarmManager + SwarmTool + registry.
/// Returns (swarm_manager, registry, notify_rx).
fn setup_repl(provider: Arc<dyn Provider>) -> (Arc<Mutex<SwarmManager>>, ToolRegistry, tokio::sync::mpsc::Receiver<flint_swarm::AgentNotification>) {
    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        config(), provider, PathBuf::from("."), "sub-agent system".into(), output_tx,
            ToolRegistry::new(), None,
    );
    let notify_rx = manager.take_notify_rx().unwrap();
    let shared = Arc::new(Mutex::new(manager));
    let mut registry = ToolRegistry::new();
    flint_swarm::register_swarm_tools(&mut registry, shared.clone(), None);
    (shared, registry, notify_rx)
}

/// Simulate the REPL's notification drain (from repl/mod.rs).
fn drain_notifications(notify_rx: &mut tokio::sync::mpsc::Receiver<flint_swarm::AgentNotification>) -> Vec<flint_swarm::AgentNotification> {
    let mut notifications = Vec::new();
    while let Ok(notif) = notify_rx.try_recv() {
        notifications.push(notif);
    }
    notifications
}

// ── Test: Full REPL turn — user asks to spawn, LLM calls swarm spawn ───

#[tokio::test]
async fn test_repl_turn_spawn() {
    // LLM responds to "spawn an agent" by calling swarm spawn, then responds with text
    let sub_provider = Arc::new(ScriptedProvider::new(vec![Action::Text("sub-agent working".into())]));
    let coordinator_provider = Arc::new(ScriptedProvider::new(vec![
        Action::Tool(ToolCall {
            id: "tc1".into(),
            name: "swarm".into(),
            input: serde_json::json!({"command": "spawn", "mode": "in-process", "prompt": "analyze code"}),
        }),
        Action::Text("I've spawned a sub-agent to analyze the code.".into()),
    ]));

    let (_swarm, registry, mut notify_rx) = setup_repl(sub_provider);
    let system = "You are a coordinator with swarm tools.";
    let c = ctx();

    // Simulate REPL: user sends message
    let mut session = Session::new();
    session.add_user("spawn an agent to analyze the code");

    let (text, stats) = run_turn(
        coordinator_provider.as_ref(), &mut session, &registry, system, &c,
        10, None, 65536, true, None,
    ).await.unwrap();

    // LLM should have processed the tool result and responded
    assert!(!text.is_empty(), "LLM should respond");
    assert!(stats.tool_calls >= 1, "should have called swarm spawn");

    // Notification should arrive (sub-agent completed)
    let notif = tokio::time::timeout(
        std::time::Duration::from_secs(10), notify_rx.recv()
    ).await.unwrap().unwrap();
    assert_eq!(notif.result.unwrap(), "sub-agent working");

    // Simulate REPL drain
    let drained = drain_notifications(&mut notify_rx);
    assert!(drained.is_empty(), "already consumed");
}

// ── Test: Full REPL multi-turn — spawn then wait then followup ──────────

#[tokio::test]
async fn test_repl_multi_turn_spawn_wait_followup() {
    // Sub-agent responds to initial task, then to followup
    let sub_provider = Arc::new(ScriptedProvider::new(vec![
        Action::Text("analysis: 5 bugs found".into()),
        Action::Text("detailed report: bug1 is critical".into()),
    ]));

    // Coordinator turn 1: calls swarm spawn
    // Coordinator turn 2: calls swarm wait (but we'll do this differently)
    // Instead, we test spawn + wait + followup through the tool directly
    let coordinator_turn1 = Arc::new(ScriptedProvider::new(vec![
        Action::Tool(ToolCall {
            id: "tc_spawn".into(),
            name: "swarm".into(),
            input: serde_json::json!({"command": "spawn", "mode": "in-process", "prompt": "find bugs"}),
        }),
        Action::Text("Spawned agent to find bugs.".into()),
    ]));

    let (swarm, registry, mut notify_rx) = setup_repl(sub_provider.clone());
    let system = "You are a coordinator.";
    let c = ctx();

    // --- Turn 1: user asks to find bugs ---
    let mut session = Session::new();
    session.add_user("find bugs in the codebase");

    let (text1, _) = run_turn(
        coordinator_turn1.as_ref(), &mut session, &registry, system, &c,
        10, None, 65536, true, None,
    ).await.unwrap();
    assert!(text1.contains("Spawned agent") || text1.contains("bugs"), "turn 1: {}", text1);

    // Wait for sub-agent to complete
    let notif = tokio::time::timeout(
        std::time::Duration::from_secs(10), notify_rx.recv()
    ).await.unwrap().unwrap();
    assert_eq!(notif.result.unwrap(), "analysis: 5 bugs found");

    // Mark completed (REPL does this after draining)
    {
        let mut m = swarm.lock().unwrap();
        m.complete_task(&notif.task_id, "analysis: 5 bugs found", true);
    }

    // --- Turn 2: user asks for details → LLM calls swarm wait ---
    let agent_id = notif.agent_id.clone();

    // The coordinator now calls swarm wait to get the result
    let coordinator_turn2 = Arc::new(ScriptedProvider::new(vec![
        Action::Tool(ToolCall {
            id: "tc_wait".into(),
            name: "swarm".into(),
            input: serde_json::json!({"command": "wait", "agent_id": agent_id, "timeout": 10}),
        }),
        Action::Text("The agent found 5 bugs. Here is the analysis.".into()),
    ]));

    session.add_user("what did the agent find?");
    let (text2, stats2) = run_turn(
        coordinator_turn2.as_ref(), &mut session, &registry, system, &c,
        10, None, 65536, true, None,
    ).await.unwrap();
    assert!(stats2.tool_calls >= 1, "turn 2 should call wait");
    assert!(
        text2.contains("5 bugs") || text2.contains("analysis"),
        "turn 2 should report results: {}", text2
    );

    // --- Turn 3: user asks for followup → LLM calls swarm followup ---
    let coordinator_turn3 = Arc::new(ScriptedProvider::new(vec![
        Action::Tool(ToolCall {
            id: "tc_followup".into(),
            name: "swarm".into(),
            input: serde_json::json!({"command": "followup", "agent_id": agent_id, "prompt": "give me details on bug1"}),
        }),
        Action::Text("The agent reports: bug1 is critical.".into()),
    ]));

    session.add_user("tell me more about bug1");
    let (text3, stats3) = run_turn(
        coordinator_turn3.as_ref(), &mut session, &registry, system, &c,
        10, None, 65536, true, None,
    ).await.unwrap();
    assert!(stats3.tool_calls >= 1, "turn 3 should call followup");
    assert!(
        text3.contains("critical") || text3.contains("bug1"),
        "turn 3 should report followup: {}", text3
    );
}

// ── Test: REPL initial message processing ───────────────────────────────

#[tokio::test]
async fn test_repl_initial_message_flow() {
    // Simulates what happens when --initial-message-file is used.
    // The REPL adds the message to session and calls run_turn.
    let sub_provider = Arc::new(ScriptedProvider::new(vec![Action::Text("done".into())]));
    let coordinator_provider = Arc::new(ScriptedProvider::new(vec![
        Action::Tool(ToolCall {
            id: "tc1".into(),
            name: "swarm".into(),
            input: serde_json::json!({"command": "spawn", "mode": "in-process", "prompt": "initial task"}),
        }),
        Action::Text("Processing your initial message. Agent spawned.".into()),
    ]));

    let (_swarm, registry, _notify_rx) = setup_repl(sub_provider);
    let system = "You are a coordinator.";
    let c = ctx();

    // Simulate REPL initial message processing (from repl/mod.rs)
    let mut session = Session::new();
    let initial_message = "start the analysis";
    session.add_user(initial_message);

    let (text, stats) = run_turn(
        coordinator_provider.as_ref(), &mut session, &registry, system, &c,
        10, None, 65536, true, None,
    ).await.unwrap();

    // The agent should have processed the initial message
    assert!(!text.is_empty(), "should respond to initial message");
    assert!(stats.tool_calls >= 1, "should call swarm");

    // Session should have the initial message and response
    assert!(session.messages.len() >= 2, "session should have user + assistant messages");
}

// ── Test: Session accumulates across turns ──────────────────────────────

#[tokio::test]
async fn test_repl_session_accumulates() {
    // Verify that the session maintains context across multiple turns
    let provider = Arc::new(ScriptedProvider::new(vec![
        Action::Text("Hello! I'm ready to help.".into()),
        Action::Text("Sure, I'll remember that.".into()),
        Action::Text("As you said, X is important.".into()),
    ]));

    let (_swarm, registry, _notify_rx) = setup_repl(
        Arc::new(ScriptedProvider::new(vec![]))
    );
    let system = "You are helpful.";
    let c = ctx();

    let mut session = Session::new();

    // Turn 1
    session.add_user("hello");
    run_turn(provider.as_ref(), &mut session, &registry, system, &c, 10, None, 65536, true, None).await.unwrap();

    // Turn 2
    session.add_user("remember: X is important");
    run_turn(provider.as_ref(), &mut session, &registry, system, &c, 10, None, 65536, true, None).await.unwrap();

    // Turn 3
    session.add_user("what did I say about X?");
    run_turn(provider.as_ref(), &mut session, &registry, system, &c, 10, None, 65536, true, None).await.unwrap();

    // Session should have accumulated all messages
    // Each turn adds: user + assistant = 2 messages per turn
    // Total: 6 messages minimum (3 user + 3 assistant)
    assert!(session.messages.len() >= 6, "session should have 6+ messages, got {}", session.messages.len());
}

// ── Test: Multiple sub-agents spawned in one turn ───────────────────────

#[tokio::test]
async fn test_repl_parallel_spawn_one_turn() {
    // LLM calls swarm spawn twice in one turn (parallel tool calls)
    // Need 2 responses — one per sub-agent
    let sub_provider = Arc::new(ScriptedProvider::new(vec![
        Action::Text("resultA".into()),
        Action::Text("resultB".into()),
    ]));

    let coordinator = Arc::new(ScriptedProvider::new(vec![
        // LLM calls two spawn tools in one response
        Action::Tool(ToolCall {
            id: "tc1".into(),
            name: "swarm".into(),
            input: serde_json::json!({"command": "spawn", "mode": "in-process", "prompt": "task A"}),
        }),
        Action::Tool(ToolCall {
            id: "tc2".into(),
            name: "swarm".into(),
            input: serde_json::json!({"command": "spawn", "mode": "in-process", "prompt": "task B"}),
        }),
        Action::Text("Spawned 2 agents for parallel work.".into()),
    ]));

    let (swarm, registry, mut notify_rx) = setup_repl(sub_provider);
    let system = "You are a coordinator.";
    let c = ctx();

    let mut session = Session::new();
    session.add_user("do A and B in parallel");

    let (text, stats) = run_turn(
        coordinator.as_ref(), &mut session, &registry, system, &c,
        10, None, 65536, true, None,
    ).await.unwrap();

    assert!(stats.tool_calls >= 2, "should call spawn twice, got {}", stats.tool_calls);
    assert!(text.contains("2 agents") || text.contains("parallel"), "response: {}", text);

    // Both notifications should arrive
    let n1 = tokio::time::timeout(std::time::Duration::from_secs(10), notify_rx.recv())
        .await.unwrap().unwrap();
    let n2 = tokio::time::timeout(std::time::Duration::from_secs(10), notify_rx.recv())
        .await.unwrap().unwrap();

    assert_ne!(n1.agent_id, n2.agent_id, "should be different agents");
    let r1 = n1.result.unwrap();
    let r2 = n2.result.unwrap();
    assert!(
        (r1 == "resultA" && r2 == "resultB") || (r1 == "resultB" && r2 == "resultA"),
        "results should be resultA and resultB, got: {:?} and {:?}", r1, r2
    );

    // Agent count should be 2
    let count = swarm.lock().unwrap().active_agent_count();
    assert_eq!(count, 2);
}

// ── Test: Sub-agent processes followup messages (multi-turn sub-agent) ──

#[tokio::test]
async fn test_sub_agent_multi_turn_via_followup() {
    // Sub-agent gets initial task, then followup messages
    let sub_provider = Arc::new(ScriptedProvider::new(vec![
        Action::Text("initial: found 3 files".into()),
        Action::Text("followup: file1.rs has a bug".into()),
        Action::Text("followup2: the bug is in line 42".into()),
    ]));

    let (swarm, registry, mut notify_rx) = setup_repl(sub_provider);

    // Spawn
    registry.execute("swarm", serde_json::json!({
        "command": "spawn", "mode": "in-process", "prompt": "find files"
    }), &ctx()).await.unwrap();

    let agent_id = swarm.lock().unwrap().agent_status()[0].0.clone();

    // Wait for initial result
    let wait_out = registry.execute("swarm", serde_json::json!({
        "command": "wait", "agent_id": agent_id, "timeout": 10
    }), &ctx()).await.unwrap();
    assert!(wait_out.text.contains("found 3 files"), "initial: {}", wait_out.text);

    // Drain notification
    drain_notifications(&mut notify_rx);

    // Assign task 1 (blocks until agent responds)
    let fu1 = registry.execute("swarm", serde_json::json!({
        "command": "assign", "agent_id": agent_id, "prompt": "which file has bugs?"
    }), &ctx()).await.unwrap();
    assert!(fu1.text.contains("file1.rs"), "assign1: {}", fu1.text);

    // Assign task 2 (blocks until agent responds)
    let fu2 = registry.execute("swarm", serde_json::json!({
        "command": "assign", "agent_id": agent_id, "prompt": "where exactly?"
    }), &ctx()).await.unwrap();
    assert!(fu2.text.contains("line 42"), "assign2: {}", fu2.text);
}

// ── Test: Notification drain with multiple agents completing ────────────

#[tokio::test]
async fn test_repl_drain_multiple_notifications() {
    let sub_provider = Arc::new(ScriptedProvider::new(vec![
        Action::Text("agent1 done".into()),
        Action::Text("agent2 done".into()),
        Action::Text("agent3 done".into()),
    ]));

    let (swarm, registry, mut notify_rx) = setup_repl(sub_provider);

    // Spawn 3 agents
    for i in 0..3 {
        registry.execute("swarm", serde_json::json!({
            "command": "spawn", "mode": "in-process", "prompt": format!("task {}", i)
        }), &ctx()).await.unwrap();
    }

    let agent_ids: Vec<String> = {
        let m = swarm.lock().unwrap();
        m.agent_status().into_iter().map(|(id, _, _)| id).collect()
    };

    // Wait for all
    for id in &agent_ids {
        registry.execute("swarm", serde_json::json!({
            "command": "wait", "agent_id": id, "timeout": 10
        }), &ctx()).await.unwrap();
    }

    // Drain all notifications at once (REPL behavior)
    let notifications = drain_notifications(&mut notify_rx);
    assert_eq!(notifications.len(), 3, "should have 3 notifications");

    // Each notification should have a unique agent_id
    let mut ids: Vec<&str> = notifications.iter().map(|n| n.agent_id.as_str()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3, "should be 3 unique agents");
}
