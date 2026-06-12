# Flint Swarm 子Agent间通信问题分析

## 问题描述

通过 `swarm spawn` 启动的两个子agent无法直接互相通信。根本原因是：**in-process子agent之间没有建立TCP连接到MessageRouter**，通信完全依赖coordinator中转。

## 架构对比：jcode vs flint

### jcode 的 swarm 架构

jcode 使用 **中心服务器 + Unix socket** 模型：

```
┌─────────────────────────────────────────┐
│           jcode Server (单进程)          │
│  ┌─────────────────────────────────┐    │
│  │  Session Manager                 │    │
│  │  ├─ Session A (coordinator)      │    │
│  │  ├─ Session B (agent)            │    │
│  │  └─ Session C (agent)            │    │
│  └─────────────────────────────────┘    │
│           ▲           ▲           ▲      │
│           │ UnixSock  │ UnixSock  │      │
└───────────┼───────────┼───────────┼──────┘
            │           │           │
         Client A    Client B    Client C
```

关键特性：
- 每个 agent 是独立的 session，通过 socket 连接到同一 server
- agent间通信通过 server 中转：`CommMessage { from_session, to_session, message }`
- 支持 channel 订阅模式（pub/sub）
- 支持共享上下文：`CommShare` / `CommRead`
- 支持结构化计划图：`CommProposePlan` / `CommAssignTask`
- 支持生命周期管理：`CommSpawn` / `CommStop` / `CommReport`

### flint 的 swarm 架构

flint 使用 **in-process tokio task + 可选TCP router** 模型：

```
┌──────────────────────────────────────────────┐
│         Coordinator Process                   │
│  ┌────────────────────────────────────────┐  │
│  │  SwarmManager                          │  │
│  │  ├─ Agent 1 (tokio task)               │  │
│  │  │   └─ mpsc::channel ←── coordinator  │  │
│  │  ├─ Agent 2 (tokio task)               │  │
│  │  │   └─ mpsc::channel ←── coordinator  │  │
│  │  └─ MessageRouter (TCP, 可选)          │  │
│  │      └─ 127.0.0.1:随机端口             │  │
│  └────────────────────────────────────────┘  │
└──────────────────────────────────────────────┘
         ▲                   ▲
         │ (无连接)          │ (无连接)
    Agent 1              Agent 2
    (无法直接通信)        (无法直接通信)
```

## 问题根因

### 1. in-process agent 不连接 router

在 `agent.rs:run_sub_agent()` 中，`router_addr` 被传入但**仅用于创建一个监听任务**：

```rust
// agent.rs:89-130
let (router_tx, mut router_rx) = mpsc::channel::<AgentRequest>(16);
if let Some(ref addr) = router_addr {
    // 创建一个 TCP 连接到 router
    tokio::spawn(async move {
        match crate::endpoint::AgentEndpoint::connect(&addr, &aid).await {
            Ok(mut ep) => {
                // 监听来自 coordinator 的消息
                loop {
                    match ep.read_message().await {
                        Ok(RouterMessage::Incoming { from, content }) => {
                            // 转发到 router_tx
                            let _ = router_tx.send(AgentRequest::Execute { ... }).await;
                        }
                        ...
                    }
                }
            }
            ...
        }
    });
}
```

问题：
- agent 确实连接到了 router 并注册了自己
- 但它**只监听 `Incoming` 消息**，不主动发送
- agent 之间没有 `send_to()` 调用 — 只有 coordinator 通过 router 发消息给 agent

### 2. coordinator 是唯一的通信枢纽

`SwarmTool` 的 `followup`/`message` 命令：

```rust
// tool.rs:192-202
"followup" => {
    // Try router first (works for all agent types)
    if let Some(ref router) = self.router {
        if router.is_connected(agent_id).await {
            router.send_to_agent(agent_id, prompt).await...
        }
    }
    // Fallback: in-process mpsc channel
    swarm.send_followup(agent_id, prompt.to_string())
}
```

只有 **coordinator → agent** 的单向通信，没有 **agent → agent** 的路径。

### 3. 子agent的工具集缺少通信能力

子agent使用 coordinator 的完整 `ToolRegistry`（包含 `SwarmTool`），理论上可以调用 `swarm` 工具。但实际上：
- 子agent调用 `swarm message agent_id=xxx` 时，走的是 coordinator 的 `SwarmManager`
- `SwarmManager` 通过 router 或 mpsc 发送，但 **router 的 `send_to_agent` 只支持 coordinator → agent 方向**
- router 的 `handle_connection` 中，agent 发送的 `Send` 消息确实能路由到其他 agent，但子agent的 `AgentEndpoint` 没有暴露给 agent 的 LLM 工具

### 4. MessageRouter 的 `Send` 支持被忽略

router.rs 中其实**已经实现了 agent → agent 的路由**：

```rust
// router.rs:182-207
RouterMessage::Send { from, to, content } => {
    let agents_map = agents.lock().await;
    if let Some(target) = agents_map.get(&to) {
        let incoming = RouterMessage::Incoming { from: from.clone(), content: content.clone() };
        // 直接路由到目标 agent
        let mut w = target.writer.lock().await;
        w.write_all(...).await;
    }
}
```

但问题是：**in-process agent 虽然连接了 router，却没有通过 router 发送消息的工具/接口**。

## 解决方案

### 方案 A：让子agent通过 router 互相通信（推荐）

**核心思路**：给子agent暴露一个 `swarm_send` 工具，让它能通过 router 直接给其他 agent 发消息。

1. **在子agent的工具集中添加通信工具**：

```rust
pub struct AgentSendTool {
    router: Arc<MessageRouter>,
    agent_id: String,
}

impl Tool for AgentSendTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "send_to_agent".into(),
            description: "Send a message to another swarm agent.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent_id": { "type": "string" },
                    "message": { "type": "string" }
                },
                "required": ["agent_id", "message"]
            }),
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let to = input["agent_id"].as_str().unwrap();
        let msg = input["message"].as_str().unwrap();
        // 通过 router 直接发送
        self.router.send_to_agent(to, msg).await?;
        Ok(ToolOutput::text(format!("sent to {}", to)))
    }
}
```

2. **在 `run_sub_agent` 中注入通信工具**：

```rust
// 在创建 registry 时，添加 agent 间通信工具
if let Some(ref router_arc) = router {
    registry.register(AgentSendTool {
        router: router_arc.clone(),
        agent_id: agent_id.clone(),
    });
    registry.register(AgentListTool { router: router_arc.clone() });
}
```

3. **确保 in-process agent 的 router 连接正确建立**：

当前代码中 `run_sub_agent` 确实连接了 router，但需要验证连接是否成功建立。建议添加重试逻辑。

### 方案 B：使用共享内存/通道（简化方案）

不依赖 TCP router，直接用 `tokio::sync::broadcast` channel：

```rust
struct SwarmManager {
    // 添加一个 broadcast channel 用于 agent 间广播
    broadcast_tx: broadcast::Sender<(String, String)>, // (from_agent, message)
}
```

每个 agent 持有 `broadcast_tx.subscribe()`，可以直接接收其他 agent 的消息。

**优点**：无需 TCP，性能更好
**缺点**：只有广播，没有点对点；不支持跨进程

### 方案 C：借鉴 jcode 的 CommMessage 模式

参考 jcode 的 `CommMessage` 协议，实现完整的 agent 间通信：

```rust
// 新增工具：comm_message
ToolDefinition {
    name: "comm_message".into(),
    description: "Send a message to another agent or broadcast to all.".into(),
    parameters: json!({
        "properties": {
            "to_agent": { "type": "string", "description": "Target agent ID, or empty for broadcast" },
            "message": { "type": "string" },
            "channel": { "type": "string", "description": "Optional channel name" }
        }
    }),
}
```

## 建议实施步骤

1. **立即修复**：在子agent的 registry 中注入 `AgentSendTool` 和 `AgentListTool`，通过已有的 MessageRouter 实现 agent → agent 通信
2. **验证 router 连接**：确保 in-process agent 的 `AgentEndpoint::connect` 成功执行
3. **添加 agent 发现**：让 agent 能查询当前在线的其他 agent（`list_agents`）
4. **可选增强**：添加 channel 订阅机制（参考 jcode 的 `ChannelIndex`）

## 参考：jcode 的关键设计模式

| 特性 | jcode | flint 现状 | flint 建议 |
|------|-------|-----------|-----------|
| Agent 注册 | AgentRegister | router Register | 已有 |
| Agent → Agent 消息 | CommMessage | ❌ 缺失 | 添加 AgentSendTool |
| 广播 | CommMessage(channel=*) | ❌ 缺失 | router Broadcast |
| 共享上下文 | CommShare/CommRead | ❌ 缺失 | 可选添加 |
| 计划协作 | CommProposePlan | ❌ 缺失 | 可选添加 |
| 生命周期 | CommSpawn/CommReport | 部分已有 | 完善 |
| Channel 订阅 | CommSubscribeChannel | ❌ 缺失 | 可选添加 |
