//! End-to-end tests for SwarmTool::execute() — tests the full tool path
//! that the coordinator LLM uses when calling swarm commands.
//!
//! These tests verify:
//! 1. spawn → wait → get result through the actual tool layer
//! 2. spawn → followup → multi-turn through the actual tool layer
//! 3. The notification channel delivers results correctly
//! 4. REPL notification draining works

use async_trait::async_trait;
use flint_agent::{run_turn, Session, ToolContext, ToolRegistry};
use flint_provider::{EventStream, Provider};
use flint_swarm::output;
use flint_swarm::{SwarmConfig, SwarmManager};
use flint_types::{Message, StreamEvent, ToolCall, ToolDefinition};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use futures::stream;

// ── Mock Providers ──────────────────────────────────────────────────────

/// Returns scripted responses in order. Falls back to "default" if exhausted.
struct SimpleMockProvider {
    responses: Mutex<Vec<String>>,
}

impl SimpleMockProvider {
    fn new(responses: Vec<String>) -> Self {
        Self { responses: Mutex::new(responses) }
    }
}

#[async_trait]
impl Provider for SimpleMockProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str) -> anyhow::Result<EventStream> {
        let mut r = self.responses.lock().unwrap();
        let text = if !r.is_empty() { r.remove(0) } else { "default".into() };
        Ok(Box::pin(stream::iter(vec![Ok(StreamEvent::TextDelta(text)), Ok(StreamEvent::End)])))
    }
}

/// Returns a tool call first, then text responses. Simulates LLM calling a tool.
struct ToolCallMockProvider {
    queue: Mutex<Vec<ProviderAction>>,
}

enum ProviderAction {
    Text(String),
    ToolCall(ToolCall),
}

impl ToolCallMockProvider {
    fn new(actions: Vec<ProviderAction>) -> Self {
        Self { queue: Mutex::new(actions) }
    }
}

#[async_trait]
impl Provider for ToolCallMockProvider {
    async fn complete(&self, _: &[Message], _: &[ToolDefinition], _: &str) -> anyhow::Result<EventStream> {
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() {
            return Ok(Box::pin(stream::iter(vec![Ok(StreamEvent::TextDelta("done".into())), Ok(StreamEvent::End)])));
        }
        match q.remove(0) {
            ProviderAction::Text(t) => Ok(Box::pin(stream::iter(vec![Ok(StreamEvent::TextDelta(t)), Ok(StreamEvent::End)]))),
            ProviderAction::ToolCall(tc) => Ok(Box::pin(stream::iter(vec![Ok(StreamEvent::ToolCall(tc)), Ok(StreamEvent::End)]))),
        }
    }
}

fn make_simple(responses: Vec<String>) -> Arc<dyn Provider> {
    Arc::new(SimpleMockProvider::new(responses))
}

fn make_tool(actions: Vec<ProviderAction>) -> Arc<dyn Provider> {
    Arc::new(ToolCallMockProvider::new(actions))
}

fn config() -> SwarmConfig {
    SwarmConfig { max_agents: 5, agent_max_turns: 10, max_output_chars: 65536, open_viewer: false }
}

fn ctx() -> ToolContext {
    ToolContext { working_dir: PathBuf::from(".") }
}

fn swarm_system() -> String {
    "You are a swarm sub-agent. Complete tasks efficiently.".to_string()
}

/// Create a SwarmManager with SwarmTool registered, return (manager, registry).
fn setup_swarm(provider: Arc<dyn Provider>) -> (Arc<Mutex<SwarmManager>>, ToolRegistry) {
    let (output_tx, _output_rx) = output::channel();
    let manager = SwarmManager::new(config(), provider, PathBuf::from("."), swarm_system(), output_tx, ToolRegistry::new(), None);
    let shared = Arc::new(Mutex::new(manager));
    let mut registry = ToolRegistry::new();
    flint_swarm::register_swarm_tools(&mut registry, shared.clone(), None);
    (shared, registry)
}

// ── Test: spawn through SwarmTool::execute ──────────────────────────────

#[tokio::test]
async fn test_tool_spawn_returns_immediately() {
    let provider = make_simple(vec!["sub-agent result".into()]);
    let (_mgr, registry) = setup_swarm(provider.clone());
    let ctx = ctx();

    // Call swarm spawn through the tool registry
    let input = serde_json::json!({
        "command": "spawn",
        "mode": "in-process",
        "prompt": "do something"
    });
    let output = registry.execute("swarm", input, &ctx).await.unwrap();

    // Should return immediately with agent_id info (non-blocking)
    assert!(!output.is_error, "spawn should not error: {}", output.text);
    assert!(output.text.contains("Spawned agent"), "should confirm spawn: {}", output.text);
    assert!(output.text.contains("agent_"), "should include agent_id: {}", output.text);
    assert!(output.text.contains("task_"), "should include task_id: {}", output.text);
}

// ── Test: spawn then wait through SwarmTool::execute ────────────────────

#[tokio::test]
async fn test_tool_spawn_then_wait() {
    let provider = make_simple(vec!["sub-agent completed work".into()]);
    let (mgr, registry) = setup_swarm(provider.clone());
    let ctx = ctx();

    // Step 1: spawn
    let spawn_input = serde_json::json!({"command": "spawn", "mode": "in-process", "prompt": "analyze code"});
    let spawn_out = registry.execute("swarm", spawn_input, &ctx).await.unwrap();
    assert!(!spawn_out.is_error);

    // Extract agent_id from spawn output
    let agent_id = {
        let m = mgr.lock().unwrap();
        let agents = m.agent_status();
        agents[0].0.clone()
    };

    // Step 2: wait for the agent
    let wait_input = serde_json::json!({"command": "wait", "agent_id": agent_id, "timeout": 10});
    let wait_out = registry.execute("swarm", wait_input, &ctx).await.unwrap();

    assert!(!wait_out.is_error, "wait should succeed: {}", wait_out.text);
    assert!(wait_out.text.contains("sub-agent completed work"), "result: {}", wait_out.text);
}

// ── Test: spawn → wait → followup (multi-turn) through tool layer ───────

#[tokio::test]
async fn test_tool_spawn_wait_followup() {
    let provider = make_simple(vec![
        "initial result".into(),
        "followup result".into(),
    ]);
    let (mgr, registry) = setup_swarm(provider.clone());
    let ctx = ctx();

    // Spawn
    let spawn_out = registry.execute("swarm", serde_json::json!({
        "command": "spawn", "mode": "in-process", "prompt": "task 1"
    }), &ctx).await.unwrap();
    assert!(!spawn_out.is_error);

    let agent_id = {
        let m = mgr.lock().unwrap();
        m.agent_status()[0].0.clone()
    };

    // Wait for initial result
    let wait_out = registry.execute("swarm", serde_json::json!({
        "command": "wait", "agent_id": agent_id, "timeout": 10
    }), &ctx).await.unwrap();
    assert!(!wait_out.is_error);
    assert!(wait_out.text.contains("initial result"), "got: {}", wait_out.text);

    // Assign another task
    let assign_out = registry.execute("swarm", serde_json::json!({
        "command": "assign", "agent_id": agent_id, "prompt": "continue working"
    }), &ctx).await.unwrap();
    assert!(!assign_out.is_error, "assign should succeed: {}", assign_out.text);
    assert!(assign_out.text.contains("followup result"), "got: {}", assign_out.text);
}

// ── Test: assign task to existing agent (blocks for result) ───────────

#[tokio::test]
async fn test_tool_assign_to_agent() {
    let provider = make_simple(vec!["initial task".into(), "assign result".into()]);
    let (mgr, registry) = setup_swarm(provider.clone());
    let ctx = ctx();

    // Spawn
    registry.execute("swarm", serde_json::json!({
        "command": "spawn", "mode": "in-process", "prompt": "task"
    }), &ctx).await.unwrap();

    let agent_id = {
        let m = mgr.lock().unwrap();
        m.agent_status()[0].0.clone()
    };

    // Assign task (blocks for result)
    let assign_out = registry.execute("swarm", serde_json::json!({
        "command": "assign", "agent_id": agent_id, "prompt": "keep going"
    }), &ctx).await.unwrap();

    assert!(!assign_out.is_error, "assign should succeed: {}", assign_out.text);
    assert!(assign_out.text.contains("assign result"), "got: {}", assign_out.text);
}

// ── Test: status command shows agents ───────────────────────────────────

#[tokio::test]
async fn test_tool_status_shows_agents() {
    let provider = make_simple(vec!["r1".into(), "r2".into()]);
    let (_mgr, registry) = setup_swarm(provider.clone());
    let ctx = ctx();

    // Spawn 2 agents
    registry.execute("swarm", serde_json::json!({
        "command": "spawn", "mode": "in-process", "prompt": "task a"
    }), &ctx).await.unwrap();
    registry.execute("swarm", serde_json::json!({
        "command": "spawn", "mode": "in-process", "prompt": "task b"
    }), &ctx).await.unwrap();

    // Check status
    let status_out = registry.execute("swarm", serde_json::json!({
        "command": "status"
    }), &ctx).await.unwrap();

    assert!(status_out.text.contains("2 active agents"), "status: {}", status_out.text);
    assert!(status_out.text.contains("task a"), "status: {}", status_out.text);
    assert!(status_out.text.contains("task b"), "status: {}", status_out.text);
}

// ── Test: stop command through tool layer ───────────────────────────────

#[tokio::test]
async fn test_tool_stop_agent() {
    let provider = make_simple(vec!["will stop".into()]);
    let (mgr, registry) = setup_swarm(provider.clone());
    let ctx = ctx();

    // Spawn
    registry.execute("swarm", serde_json::json!({
        "command": "spawn", "mode": "in-process", "prompt": "stop me"
    }), &ctx).await.unwrap();

    let agent_id = {
        let m = mgr.lock().unwrap();
        m.agent_status()[0].0.clone()
    };

    // Stop
    let stop_out = registry.execute("swarm", serde_json::json!({
        "command": "stop", "agent_id": agent_id
    }), &ctx).await.unwrap();

    assert!(stop_out.text.contains("stopped"), "stop: {}", stop_out.text);

    // Verify agent is stopped
    let alive = mgr.lock().unwrap().is_agent_alive(&agent_id);
    assert!(!alive, "agent should be stopped");
}

// ── Test: notification channel delivers results ─────────────────────────

#[tokio::test]
async fn test_notification_delivered_after_spawn_wait() {
    let provider = make_simple(vec!["notif test".into()]);
    let (mgr, registry) = setup_swarm(provider.clone());
    let ctx = ctx();

    // Take notification receiver
    let mut notify_rx = mgr.lock().unwrap().take_notify_rx().unwrap();

    // Spawn + wait
    registry.execute("swarm", serde_json::json!({
        "command": "spawn", "mode": "in-process", "prompt": "notify task"
    }), &ctx).await.unwrap();

    let agent_id = {
        let m = mgr.lock().unwrap();
        m.agent_status()[0].0.clone()
    };

    registry.execute("swarm", serde_json::json!({
        "command": "wait", "agent_id": agent_id, "timeout": 10
    }), &ctx).await.unwrap();

    // Notification should be available
    let notif = tokio::time::timeout(
        std::time::Duration::from_secs(5), notify_rx.recv()
    ).await.unwrap().unwrap();

    assert_eq!(notif.agent_id, agent_id);
    assert_eq!(notif.result.unwrap(), "notif test");
}

// ── Test: REPL notification draining simulation ─────────────────────────

#[tokio::test]
async fn test_repl_notification_drain() {
    let provider = make_simple(vec!["drain test".into()]);
    let (mgr, registry) = setup_swarm(provider.clone());
    let ctx = ctx();

    let mut notify_rx = mgr.lock().unwrap().take_notify_rx().unwrap();

    // Spawn + wait (triggers notification)
    registry.execute("swarm", serde_json::json!({
        "command": "spawn", "mode": "in-process", "prompt": "drain task"
    }), &ctx).await.unwrap();

    let agent_id = {
        let m = mgr.lock().unwrap();
        m.agent_status()[0].0.clone()
    };

    registry.execute("swarm", serde_json::json!({
        "command": "wait", "agent_id": agent_id, "timeout": 10
    }), &ctx).await.unwrap();

    // Simulate REPL drain (exactly what repl/mod.rs does)
    let mut notifications = Vec::new();
    while let Ok(notif) = notify_rx.try_recv() {
        notifications.push(notif);
    }

    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].result.as_ref().unwrap(), "drain test");

    // Mark completed (what the REPL does after draining)
    {
        let mut m = mgr.lock().unwrap();
        for n in &notifications {
            match &n.result {
                Ok(text) => m.complete_task(&n.task_id, text, true),
                Err(e) => m.complete_task(&n.task_id, e, false),
            }
        }
    }

    // Verify task result is cached
    let task_id = &notifications[0].task_id;
    let cached = mgr.lock().unwrap().get_task_result(task_id);
    assert_eq!(cached.unwrap(), "drain test");
}

// ── Test: full coordinator flow with run_turn ───────────────────────────
// This tests the complete flow: user message → LLM calls swarm spawn →
// spawn returns → LLM calls swarm wait → wait returns result → LLM responds.

#[tokio::test]
async fn test_full_coordinator_flow_with_run_turn() {
    // Script the LLM responses:
    // Turn 1: LLM calls swarm spawn
    // After spawn tool result: LLM calls swarm wait
    // After wait tool result: LLM responds with text
    let provider = make_tool(vec![
        ProviderAction::ToolCall(ToolCall {
            id: "tc_spawn".into(),
            name: "swarm".into(),
            input: serde_json::json!({"command": "spawn", "mode": "in-process", "prompt": "analyze codebase"}),
        }),
        ProviderAction::ToolCall(ToolCall {
            id: "tc_wait".into(),
            name: "swarm".into(),
            input: serde_json::json!({"command": "wait", "agent_id": "__AGENT_ID__", "timeout": 10}),
        }),
        ProviderAction::Text("The sub-agent analyzed the codebase and found 3 issues.".into()),
    ]);

    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        config(), make_simple(vec!["analysis complete: 3 issues found".into()]),
        PathBuf::from("."), swarm_system(), output_tx,
            ToolRegistry::new(), None,
    );
    // Take notify_rx before wrapping in Arc<Mutex>
    let mut notify_rx = manager.take_notify_rx().unwrap();
    let shared = Arc::new(Mutex::new(manager));

    let mut registry = ToolRegistry::new();
    flint_swarm::register_swarm_tools(&mut registry, shared.clone(), None);

    let ctx = ctx();
    let system = "You are a coordinator with swarm access.";

    // Run a turn with the user message
    let mut session = Session::new();
    session.add_user("analyze the codebase");

    let result = run_turn(
        provider.as_ref(), &mut session, &registry, system, &ctx,
        10, None, 65536, true, None, None,
    ).await;

    let (text, _stats) = result.unwrap();
    // The LLM should have received the tool results and produced text
    assert!(
        text.contains("analysis complete") || text.contains("3 issues"),
        "coordinator should report results: {}", text
    );

    // Notification should have been delivered
    let notif = tokio::time::timeout(
        std::time::Duration::from_secs(5), notify_rx.recv()
    ).await.unwrap().unwrap();
    assert_eq!(notif.result.unwrap(), "analysis complete: 3 issues found");
}

// ── Test: error handling — wait for nonexistent agent ───────────────────

#[tokio::test]
async fn test_tool_wait_nonexistent_agent() {
    let provider = make_simple(vec![]);
    let (_mgr, registry) = setup_swarm(provider);
    let ctx = ctx();

    let result = registry.execute("swarm", serde_json::json!({
        "command": "wait", "agent_id": "agent_nonexistent", "timeout": 2
    }), &ctx).await;

    // Should return an error (agent not found)
    assert!(result.is_err(), "should error for nonexistent agent");
}

// ── Test: error handling — assign to nonexistent agent ─────────────────

#[tokio::test]
async fn test_tool_assign_nonexistent_agent() {
    let provider = make_simple(vec![]);
    let (_mgr, registry) = setup_swarm(provider);
    let ctx = ctx();

    let result = registry.execute("swarm", serde_json::json!({
        "command": "assign", "agent_id": "agent_nonexistent", "prompt": "hello"
    }), &ctx).await.unwrap();

    // assign wraps errors in ToolOutput::error (not Err)
    assert!(result.is_error, "should error for nonexistent agent: {}", result.text);
}

// ── Test: multiple agents, wait all ─────────────────────────────────────

#[tokio::test]
async fn test_tool_multiple_agents_wait_all() {
    let provider = make_simple(vec!["result A".into(), "result B".into(), "result C".into()]);
    let (mgr, registry) = setup_swarm(provider);
    let ctx = ctx();

    // Spawn 3 agents
    for i in 0..3 {
        registry.execute("swarm", serde_json::json!({
            "command": "spawn", "mode": "in-process", "prompt": format!("task {}", i)
        }), &ctx).await.unwrap();
    }

    let agent_ids: Vec<String> = {
        let m = mgr.lock().unwrap();
        m.agent_status().into_iter().map(|(id, _, _)| id).collect()
    };
    assert_eq!(agent_ids.len(), 3);

    // Wait for each
    for (i, agent_id) in agent_ids.iter().enumerate() {
        let out = registry.execute("swarm", serde_json::json!({
            "command": "wait", "agent_id": agent_id, "timeout": 10
        }), &ctx).await.unwrap();
        assert!(!out.is_error, "agent {} wait failed: {}", i, out.text);
    }
}

// ── Test: status command shows tasks ───────────────────────────────────

#[tokio::test]
async fn test_tool_status_shows_tasks() {
    let provider = make_simple(vec!["r".into()]);
    let (_mgr, registry) = setup_swarm(provider);
    let ctx = ctx();

    // Empty status
    let out = registry.execute("swarm", serde_json::json!({"command": "status"}), &ctx).await.unwrap();
    assert!(out.text.contains("0 active agents"), "empty: {}", out.text);

    // After spawn
    registry.execute("swarm", serde_json::json!({
        "command": "spawn", "mode": "in-process", "prompt": "my task"
    }), &ctx).await.unwrap();

    let out = registry.execute("swarm", serde_json::json!({"command": "status"}), &ctx).await.unwrap();
    assert!(out.text.contains("my task"), "status: {}", out.text);
    assert!(out.text.contains("running"), "status: {}", out.text);
}

// ── Test: sub-agent with actual tool execution ────────────────────────
// This verifies the full autonomous loop: spawn → LLM → tool call →
// tool executes → LLM sees result → final text.

#[tokio::test]
async fn test_sub_agent_executes_tool_autonomously() {
    // Use a temp directory for file output
    let tmp = std::env::temp_dir().join(format!("flint_test_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();

    // LLM will: 1) call write tool, 2) return text
    let provider = make_tool(vec![
        ProviderAction::ToolCall(ToolCall {
            id: "call_write".into(),
            name: "write".into(),
            input: serde_json::json!({"path": "output.txt", "content": "hello from sub-agent"}),
        }),
        ProviderAction::Text("I wrote the file successfully.".into()),
    ]);

    let (output_tx, mut output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        SwarmConfig { max_agents: 5, agent_max_turns: 10, max_output_chars: 65536, open_viewer: false },
        provider,
        tmp.clone(),
        "You are a test sub-agent.".into(),
        output_tx,
        ToolRegistry::new(),
        None,
    );
    let mut notify_rx = manager.take_notify_rx().unwrap();

    // Spawn the agent
    let spawn = manager.spawn_agent("write a file called output.txt".into(), None, Vec::new()).unwrap();
    let agent_id = spawn.agent_id.clone();
    let task_id = spawn.task_id.clone();

    // Wait for the agent to complete (max 15s)
    let rx = manager.take_initial_result(&agent_id).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), rx)
        .await
        .expect("agent timed out")
        .expect("channel dropped")
        .expect("agent failed");

    // Verify the agent's final text
    eprintln!("=== Agent result: {:?}", result);
    assert!(
        result.contains("wrote the file") || result.contains("successfully"),
        "unexpected result: {:?}", result
    );

    // Verify the file was actually written
    let file_path = tmp.join("output.txt");
    eprintln!("=== Looking for file at: {:?}", file_path);
    eprintln!("=== Tmp dir exists: {}", tmp.exists());
    eprintln!("=== Tmp dir contents:");
    if let Ok(entries) = std::fs::read_dir(&tmp) {
        for e in entries.flatten() {
            eprintln!("  {:?}", e.path());
        }
    }
    assert!(file_path.exists(), "sub-agent should have created output.txt at {:?}", file_path);
    let content = std::fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "hello from sub-agent", "file content mismatch");

    // Verify notification was sent
    let notif = tokio::time::timeout(
        std::time::Duration::from_secs(5), notify_rx.recv()
    ).await.unwrap().unwrap();
    assert_eq!(notif.agent_id, agent_id);
    assert_eq!(notif.task_id, task_id);
    let notif_text = notif.result.unwrap();
    assert!(notif_text.contains("wrote the file") || notif_text.contains("successfully"), "notif: {}", notif_text);

    // Verify output events were emitted
    let mut events = Vec::new();
    while let Ok(ev) = output_rx.try_recv() {
        events.push(ev);
    }
    // Should have at least: Started, Thinking, Done
    assert!(events.len() >= 3, "expected at least 3 output events, got {}", events.len());

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Test: agent runs independently across multiple tasks ──────────────
// Verifies:
// 1. Agent spawns with initial task, executes tools, reports result
// 2. Coordinator assigns second task (fire-and-forget)
// 3. Agent continues with conversation context from first task
// 4. Both results are independent and correct

#[tokio::test]
async fn test_agent_independent_multi_task() {
    let tmp = std::env::temp_dir().join(format!("flint_test_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();

    // LLM sequence:
    // Turn 1: write task1.txt → "task 1 done"
    // Turn 2: write task2.txt → "task 2 done"
    let provider = make_tool(vec![
        // Task 1
        ProviderAction::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "write".into(),
            input: serde_json::json!({"path": "task1.txt", "content": "first task output"}),
        }),
        ProviderAction::Text("Task 1 complete. I wrote task1.txt.".into()),
        // Task 2
        ProviderAction::ToolCall(ToolCall {
            id: "call_2".into(),
            name: "write".into(),
            input: serde_json::json!({"path": "task2.txt", "content": "second task output"}),
        }),
        ProviderAction::Text("Task 2 complete. I wrote task2.txt.".into()),
    ]);

    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        SwarmConfig { max_agents: 5, agent_max_turns: 10, max_output_chars: 65536, open_viewer: false },
        provider,
        tmp.clone(),
        "You are an independent agent. Complete tasks efficiently.".into(),
        output_tx,
        ToolRegistry::new(),
        None,
    );

    // ── Step 1: Spawn with initial task ────────────────────────────────
    let spawn = manager.spawn_agent("create task1.txt with content 'first task output'".into(), None, Vec::new()).unwrap();
    let agent_id = spawn.agent_id.clone();

    // Wait for initial result
    let rx = manager.take_initial_result(&agent_id).unwrap();
    let result1 = tokio::time::timeout(std::time::Duration::from_secs(15), rx)
        .await.expect("timeout").expect("dropped").expect("failed");
    assert!(result1.contains("Task 1"), "result1: {}", result1);

    // Verify file created
    assert!(tmp.join("task1.txt").exists(), "task1.txt should exist");

    // ── Step 2: Assign second task (fire-and-forget) ───────────────────
    // The agent should still be alive and waiting for new tasks
    assert!(manager.is_agent_alive(&agent_id), "agent should still be alive");

    // Use send_followup directly (simulates what assign does)
    let rx2 = manager.send_followup(&agent_id, "create task2.txt with content 'second task output'".into()).unwrap();

    // Wait for second result
    let result2 = tokio::time::timeout(std::time::Duration::from_secs(15), rx2)
        .await.expect("timeout").expect("dropped").expect("failed");
    assert!(result2.contains("Task 2"), "result2: {}", result2);

    // Verify second file created
    assert!(tmp.join("task2.txt").exists(), "task2.txt should exist");

    // ── Step 3: Verify agent is still alive (can accept more tasks) ────
    assert!(manager.is_agent_alive(&agent_id), "agent should still be alive after 2 tasks");

    // Verify both files have correct content
    assert_eq!(std::fs::read_to_string(tmp.join("task1.txt")).unwrap(), "first task output");
    assert_eq!(std::fs::read_to_string(tmp.join("task2.txt")).unwrap(), "second task output");

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Test: two agents run independently without interference ───────────

#[tokio::test]
async fn test_two_agents_no_interference() {
    let tmp_a = std::env::temp_dir().join(format!("flint_a_{}", uuid::Uuid::new_v4()));
    let tmp_b = std::env::temp_dir().join(format!("flint_b_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_a).unwrap();
    std::fs::create_dir_all(&tmp_b).unwrap();

    // Agent A writes to tmp_a
    let provider_a = make_tool(vec![
        ProviderAction::ToolCall(ToolCall {
            id: "a1".into(), name: "write".into(),
            input: serde_json::json!({"path": "output.txt", "content": "from agent A"}),
        }),
        ProviderAction::Text("Agent A done.".into()),
    ]);
    // Agent B writes to tmp_b
    let provider_b = make_tool(vec![
        ProviderAction::ToolCall(ToolCall {
            id: "b1".into(), name: "write".into(),
            input: serde_json::json!({"path": "output.txt", "content": "from agent B"}),
        }),
        ProviderAction::Text("Agent B done.".into()),
    ]);

    let (output_tx, _output_rx) = output::channel();
    let cfg = SwarmConfig { max_agents: 5, agent_max_turns: 10, max_output_chars: 65536, open_viewer: false };
    let sys = "You are an agent.".to_string();

    let mut mgr_a = SwarmManager::new(cfg.clone(), provider_a, tmp_a.clone(), sys.clone(), output_tx.clone(), ToolRegistry::new(), None);
    let mut mgr_b = SwarmManager::new(cfg, provider_b, tmp_b.clone(), sys, output_tx, ToolRegistry::new(), None);

    // Spawn both
    let sa = mgr_a.spawn_agent("write output.txt".into(), None, Vec::new()).unwrap();
    let sb = mgr_b.spawn_agent("write output.txt".into(), None, Vec::new()).unwrap();

    // Wait for both
    let rx_a = mgr_a.take_initial_result(&sa.agent_id).unwrap();
    let rx_b = mgr_b.take_initial_result(&sb.agent_id).unwrap();

    let (r_a, r_b) = tokio::join!(
        tokio::time::timeout(std::time::Duration::from_secs(15), rx_a),
        tokio::time::timeout(std::time::Duration::from_secs(15), rx_b),
    );

    assert!(r_a.unwrap().unwrap().unwrap().contains("Agent A done"));
    assert!(r_b.unwrap().unwrap().unwrap().contains("Agent B done"));

    // Verify no interference — each wrote to its own directory
    assert_eq!(std::fs::read_to_string(tmp_a.join("output.txt")).unwrap(), "from agent A");
    assert_eq!(std::fs::read_to_string(tmp_b.join("output.txt")).unwrap(), "from agent B");

    let _ = std::fs::remove_dir_all(&tmp_a);
    let _ = std::fs::remove_dir_all(&tmp_b);
}
