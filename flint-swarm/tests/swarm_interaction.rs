//! Integration tests for swarm multi-turn interaction.
//!
//! These tests verify that:
//! 1. Sub-agents can be spawned and return results
//! 2. Follow-up messages work (multi-turn conversation)
//! 3. Notifications are delivered correctly
//! 4. The wait command retrieves results

use flint_agent::ToolRegistry;
use flint_provider::{EventStream, Provider};
use flint_swarm::output;
use flint_swarm::{SwarmConfig, SwarmManager};
use flint_types::{Message, StreamEvent, ToolCall, ToolDefinition};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use futures::stream;

/// A scripted response that can be either text or a tool call.
enum ScriptedResponse {
    Text(String),
    ToolCall { id: String, name: String, input: serde_json::Value },
}

/// Mock provider that returns scripted responses including tool calls.
struct ScriptedProvider {
    responses: Mutex<Vec<ScriptedResponse>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ScriptedResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
    ) -> anyhow::Result<EventStream> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Ok(Box::pin(stream::iter(vec![
                Ok(StreamEvent::TextDelta("default".to_string())),
                Ok(StreamEvent::End),
            ])));
        }
        let resp = responses.remove(0);
        let events = match resp {
            ScriptedResponse::Text(text) => {
                vec![Ok(StreamEvent::TextDelta(text)), Ok(StreamEvent::End)]
            }
            ScriptedResponse::ToolCall { id, name, input } => {
                vec![
                    Ok(StreamEvent::ToolCall(ToolCall { id, name, input })),
                    Ok(StreamEvent::End),
                ]
            }
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

/// Mock provider that returns scripted responses.
struct MockProvider {
    responses: Mutex<Vec<String>>,
}

impl MockProvider {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
    ) -> anyhow::Result<EventStream> {
        let mut responses = self.responses.lock().unwrap();
        let text = if !responses.is_empty() {
            responses.remove(0)
        } else {
            "default response".to_string()
        };
        let events = vec![Ok(StreamEvent::TextDelta(text)), Ok(StreamEvent::End)];
        Ok(Box::pin(stream::iter(events)))
    }
}

fn make_provider(responses: Vec<String>) -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(responses))
}

fn test_config() -> SwarmConfig {
    SwarmConfig {
        max_agents: 5,
        agent_max_turns: 10,
        max_output_chars: 65536,
        open_viewer: false,
    }
}

#[tokio::test]
async fn test_spawn_and_wait() {
    // Spawn a sub-agent that returns "hello from agent"
    // Then wait for the result
    let provider = make_provider(vec!["hello from agent".to_string()]);
    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        test_config(),
        provider,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx,
            ToolRegistry::new(), None,
    );

    // Spawn
    let spawn_result = manager.spawn_agent("test task".to_string(), None, Vec::new()).unwrap();
    let agent_id = spawn_result.agent_id.clone();

    // The agent should be alive
    assert!(manager.is_agent_alive(&agent_id));

    // Wait for the initial result
    let rx = manager.take_initial_result(&agent_id).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), rx).await;
    match result {
        Ok(Ok(Ok(text))) => {
            assert_eq!(text, "hello from agent");
        }
        Ok(Ok(Err(e))) => panic!("agent failed: {}", e),
        Ok(Err(_)) => panic!("channel dropped"),
        Err(_) => panic!("timeout waiting for result"),
    }

    // Task should be completed now (via notification)
    // Note: complete_task is called by the REPL, not automatically
    // But the result was delivered through the oneshot channel
}

#[tokio::test]
async fn test_followup_multi_turn() {
    // Test multi-turn conversation with follow-up messages
    let provider = make_provider(vec![
        "turn 1 response".to_string(),
        "turn 2 response".to_string(),
        "turn 3 response".to_string(),
    ]);
    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        test_config(),
        provider,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx,
            ToolRegistry::new(), None,
    );

    // Spawn
    let spawn_result = manager.spawn_agent("initial task".to_string(), None, Vec::new()).unwrap();
    let agent_id = spawn_result.agent_id;

    // Wait for initial result
    let rx = manager.take_initial_result(&agent_id).unwrap();
    let result1 = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result1, "turn 1 response");

    // Send follow-up 1
    let rx2 = manager.send_followup(&agent_id, "follow-up 1".to_string()).unwrap();
    let result2 = tokio::time::timeout(std::time::Duration::from_secs(10), rx2)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result2, "turn 2 response");

    // Send follow-up 2
    let rx3 = manager.send_followup(&agent_id, "follow-up 2".to_string()).unwrap();
    let result3 = tokio::time::timeout(std::time::Duration::from_secs(10), rx3)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result3, "turn 3 response");
}

#[tokio::test]
async fn test_notification_channel() {
    // Test that notifications are delivered through the channel
    let provider = make_provider(vec!["notification test result".to_string()]);
    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        test_config(),
        provider,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx,
            ToolRegistry::new(), None,
    );

    // Take the notification receiver
    let mut notify_rx = manager.take_notify_rx().unwrap();

    // Spawn
    let spawn_result = manager.spawn_agent("notify task".to_string(), None, Vec::new()).unwrap();
    let agent_id = spawn_result.agent_id;
    let task_id = spawn_result.task_id;

    // Wait for the initial result (this consumes the oneshot)
    let rx = manager.take_initial_result(&agent_id).unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), rx).await;

    // Now check the notification channel
    let notification = tokio::time::timeout(std::time::Duration::from_secs(5), notify_rx.recv())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(notification.agent_id, agent_id);
    assert_eq!(notification.task_id, task_id);
    assert_eq!(notification.result.unwrap(), "notification test result");
}

#[tokio::test]
async fn test_multiple_agents_parallel() {
    // Test spawning multiple agents and waiting for all results
    let provider = make_provider(vec![
        "agent 1 result".to_string(),
        "agent 2 result".to_string(),
        "agent 3 result".to_string(),
    ]);
    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        test_config(),
        provider,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx,
            ToolRegistry::new(), None,
    );

    // Spawn 3 agents
    let r1 = manager.spawn_agent("task 1".to_string(), None, Vec::new()).unwrap();
    let r2 = manager.spawn_agent("task 2".to_string(), None, Vec::new()).unwrap();
    let r3 = manager.spawn_agent("task 3".to_string(), None, Vec::new()).unwrap();

    // Wait for all results in parallel
    let rx1 = manager.take_initial_result(&r1.agent_id).unwrap();
    let rx2 = manager.take_initial_result(&r2.agent_id).unwrap();
    let rx3 = manager.take_initial_result(&r3.agent_id).unwrap();

    let (res1, res2, res3) = tokio::join!(
        tokio::time::timeout(std::time::Duration::from_secs(10), rx1),
        tokio::time::timeout(std::time::Duration::from_secs(10), rx2),
        tokio::time::timeout(std::time::Duration::from_secs(10), rx3),
    );

    let text1 = res1.unwrap().unwrap().unwrap();
    let text2 = res2.unwrap().unwrap().unwrap();
    let text3 = res3.unwrap().unwrap().unwrap();

    assert_eq!(text1, "agent 1 result");
    assert_eq!(text2, "agent 2 result");
    assert_eq!(text3, "agent 3 result");
}

#[tokio::test]
async fn test_wait_after_completion() {
    // Test that wait returns immediately if agent already completed
    let provider = make_provider(vec!["already done".to_string()]);
    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        test_config(),
        provider,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx,
            ToolRegistry::new(), None,
    );

    let spawn_result = manager.spawn_agent("quick task".to_string(), None, Vec::new()).unwrap();
    let agent_id = spawn_result.agent_id;

    // Wait for completion
    let rx = manager.take_initial_result(&agent_id).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result, "already done");

    // Mark task as completed in the manager
    manager.complete_task(&spawn_result.task_id, &result, true);

    // Now try to get the result from the task registry
    let cached = manager.get_task_result(&spawn_result.task_id);
    assert_eq!(cached.unwrap(), "already done");
}

#[tokio::test]
async fn test_stop_agent() {
    // Test that stopping an agent works
    let provider = make_provider(vec!["will be stopped".to_string()]);
    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        test_config(),
        provider,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx,
            ToolRegistry::new(), None,
    );

    let spawn_result = manager.spawn_agent("stop task".to_string(), None, Vec::new()).unwrap();
    let agent_id = spawn_result.agent_id;

    // Stop the agent
    manager.stop_agent(&agent_id).unwrap();

    // Agent should no longer be alive
    assert!(!manager.is_agent_alive(&agent_id));
}

#[tokio::test]
async fn test_agent_count() {
    // Test active agent count
    let provider = make_provider(vec![
        "a".to_string(), "b".to_string(), "c".to_string(),
    ]);
    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        test_config(),
        provider,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx,
            ToolRegistry::new(), None,
    );

    assert_eq!(manager.active_agent_count(), 0);

    let r1 = manager.spawn_agent("t1".to_string(), None, Vec::new()).unwrap();
    assert_eq!(manager.active_agent_count(), 1);

    let r2 = manager.spawn_agent("t2".to_string(), None, Vec::new()).unwrap();
    assert_eq!(manager.active_agent_count(), 2);

    manager.stop_agent(&r1.agent_id).unwrap();
    assert_eq!(manager.active_agent_count(), 1);

    manager.stop_agent(&r2.agent_id).unwrap();
    assert_eq!(manager.active_agent_count(), 0);
}

/// End-to-end test: sub-agent calls request_input, REPL sends response,
/// agent continues and completes. This tests the full inline agent loop
/// with non-blocking input request handling.
#[tokio::test]
async fn test_input_request_response_e2e() {
    // Turn 1: LLM calls request_input tool
    // Turn 2: LLM sees user response and returns final text
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedResponse::ToolCall {
            id: "call_1".to_string(),
            name: "request_input".to_string(),
            input: serde_json::json!({"prompt": "What is your name?"}),
        },
        ScriptedResponse::Text("Nice to meet you, Alice!".to_string()),
    ]));
    let (output_tx, _output_rx) = output::channel();
    let mut manager = SwarmManager::new(
        test_config(),
        provider,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx,
        ToolRegistry::new(),
        None,
    );

    // Spawn the sub-agent
    let spawn_result = manager.spawn_agent("introduce yourself".to_string(), None, Vec::new()).unwrap();
    let agent_id = spawn_result.agent_id.clone();

    // Wait for the agent to send an input request
    // Poll drain_input_requests until we get one (max 10s)
    let mut input_request = None;
    for _ in 0..200 {
        let requests = manager.drain_input_requests();
        if !requests.is_empty() {
            input_request = Some(requests.into_iter().next().unwrap());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let req = input_request.expect("agent should have sent an input request within 10s");
    assert_eq!(req.agent_id, agent_id);
    assert_eq!(req.prompt, "What is your name?");

    // Send the user's response back to the agent
    manager
        .send_input_response(&agent_id, "Alice".to_string())
        .await
        .unwrap();

    // Wait for the agent to complete
    let rx = manager.take_initial_result(&agent_id).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
        .await
        .expect("agent should complete within 30s")
        .expect("channel should not be dropped")
        .expect("agent should succeed");

    assert_eq!(result, "Nice to meet you, Alice!");
}

/// Test that two sub-agents can independently request input and receive
/// responses without blocking each other.
#[tokio::test]
async fn test_two_agents_input_requests() {
    // Both agents call request_input, then complete
    let provider_a = Arc::new(ScriptedProvider::new(vec![
        ScriptedResponse::ToolCall {
            id: "call_a1".to_string(),
            name: "request_input".to_string(),
            input: serde_json::json!({"prompt": "Agent A question?"}),
        },
        ScriptedResponse::Text("Agent A done".to_string()),
    ]));
    let provider_b = Arc::new(ScriptedProvider::new(vec![
        ScriptedResponse::ToolCall {
            id: "call_b1".to_string(),
            name: "request_input".to_string(),
            input: serde_json::json!({"prompt": "Agent B question?"}),
        },
        ScriptedResponse::Text("Agent B done".to_string()),
    ]));

    let (output_tx, _output_rx) = output::channel();

    // Spawn agent A
    let mut manager_a = SwarmManager::new(
        test_config(),
        provider_a,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx.clone(),
        ToolRegistry::new(),
        None,
    );
    let spawn_a = manager_a.spawn_agent("task A".to_string(), None, Vec::new()).unwrap();

    // Spawn agent B
    let mut manager_b = SwarmManager::new(
        test_config(),
        provider_b,
        PathBuf::from("."),
        "test system".to_string(),
        output_tx,
        ToolRegistry::new(),
        None,
    );
    let spawn_b = manager_b.spawn_agent("task B".to_string(), None, Vec::new()).unwrap();

    // Wait for both to send input requests
    let mut req_a = None;
    let mut req_b = None;
    for _ in 0..200 {
        if req_a.is_none() {
            let reqs = manager_a.drain_input_requests();
            if !reqs.is_empty() {
                req_a = Some(reqs.into_iter().next().unwrap());
            }
        }
        if req_b.is_none() {
            let reqs = manager_b.drain_input_requests();
            if !reqs.is_empty() {
                req_b = Some(reqs.into_iter().next().unwrap());
            }
        }
        if req_a.is_some() && req_b.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(req_a.is_some(), "Agent A should have sent input request");
    assert!(req_b.is_some(), "Agent B should have sent input request");

    // Respond to both
    manager_a
        .send_input_response(&spawn_a.agent_id, "answer A".to_string())
        .await
        .unwrap();
    manager_b
        .send_input_response(&spawn_b.agent_id, "answer B".to_string())
        .await
        .unwrap();

    // Wait for both to complete
    let rx_a = manager_a.take_initial_result(&spawn_a.agent_id).unwrap();
    let rx_b = manager_b.take_initial_result(&spawn_b.agent_id).unwrap();

    let (result_a, result_b) = tokio::join!(
        tokio::time::timeout(std::time::Duration::from_secs(30), rx_a),
        tokio::time::timeout(std::time::Duration::from_secs(30), rx_b),
    );

    let text_a = result_a.unwrap().unwrap().unwrap();
    let text_b = result_b.unwrap().unwrap().unwrap();

    assert_eq!(text_a, "Agent A done");
    assert_eq!(text_b, "Agent B done");
}
