# Flint Memory 系统综合分析报告

## 1. 系统定位与核心作用

Flint Memory 是 flint 编码 Agent 的**认知基础设施**。它解决了一个根本问题：**LLM 是无状态的**。每次 API 调用都是独立的，模型无法记住用户偏好、无法从错误中学习、无法积累项目知识。Memory 系统通过三层记忆架构，让 Agent 从"无状态工具"进化为"有记忆的助手"。

### 核心价值

| 问题 | Memory 解决方案 |
|------|----------------|
| LLM 每次调用独立 | Core Memory 每轮注入 system prompt，保持上下文连贯 |
| 无法记住用户偏好 | 自动提取并持久化 Preference 类型记忆 |
| 无法从错误中学习 | Correction 类型记忆（搜索加分最高、半衰期最长） |
| 无法积累项目知识 | Project 作用域存储项目特定知识 |
| 对话窗口有限 | Archival Memory 按需检索，突破上下文窗口限制 |

---

## 2. 三层架构详解

```
┌──────────────────────────────────────────────────────────────────┐
│                   System Prompt (每轮注入)                         │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  Layer 1: Core Memory (核心记忆)                            │  │
│  │  persona / user / project 三个 Block                        │  │
│  │  → 始终在 system prompt 中可见，Agent 可通过 tool call 读写   │  │
│  └────────────────────────────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────────────┤
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  Layer 2: Archival Memory (归档记忆)                        │  │
│  │  长期持久化 JSON 存储，关键词 TF-IDF 检索                    │  │
│  │  → 按需检索最相关条目注入对话上下文                           │  │
│  └────────────────────────────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────────────┤
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  Layer 3: Recall Memory (召回记忆)                          │  │
│  │  会话内临时事实，仅存内存，不跨会话持久化                      │  │
│  │  → 结构已定义 (RecallEntry)，功能尚待完整实现                 │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

设计灵感来自 **Letta/MemGPT** 的分层记忆概念。

---

## 3. 源码模块架构

```
flint-memory/src/
├── lib.rs       → 模块入口，三层架构文档注释，公共类型 re-export
├── types.rs     → 全部核心数据结构（CoreBlock, MemoryEntry, MemoryCategory, TrustLevel...）
├── core.rs      → Layer 1 CoreMemory：加载、保存、更新、渲染 core blocks
├── store.rs     → Layer 2 MemoryStore：JSON 持久化、路径管理、去重检测
├── search.rs    → TF-IDF 搜索引擎：评分公式、分词、停用词过滤
└── manager.rs   → MemoryManager：统一编排入口，串联三层，暴露高层 API

flint-cli/src/
├── tools.rs     → 5 个 Memory Tool 实现（register_memory_tools()）
├── prompt.rs    → System Prompt 构建（append_core_memory()）
└── repl/mod.rs  → REPL 主循环：动态记忆注入 + 自动提取
```

---

## 4. 数据模型

### 4.1 CoreBlock — 核心记忆块 (Layer 1)

```rust
pub struct CoreBlock {
    pub label: String,              // 标识："persona" | "user" | "project"
    pub content: String,            // 文本内容
    pub limit: usize,               // 字符上限（默认 2000）
    pub read_only: bool,            // 是否只读
    pub updated_at: DateTime<Utc>,  // 最后更新时间
}
```

**默认三个 Block：**
- `persona`：Agent 身份和行为准则（"You are flint..."）
- `user`：用户偏好（初始为空，通过交互学习）
- `project`：当前项目上下文（初始为空，通过探索发现）

渲染格式（注入 system prompt）：
```
[persona]
You are flint, a fast and focused coding agent...

[user]
(User prefers concise answers in Chinese...)

[project]
(Working on flint-memory, a Rust memory module...)
```

### 4.2 MemoryEntry — 归档记忆条目 (Layer 2)

```rust
pub struct MemoryEntry {
    pub id: String,                    // "mem_{uuid}" 格式
    pub category: MemoryCategory,      // 分类枚举
    pub content: String,               // 记忆内容
    pub tags: Vec<String>,             // 搜索标签
    pub scope: MemoryScope,            // Global | Project
    pub trust: TrustLevel,             // High | Medium | Low
    pub access_count: u32,             // 访问计数
    pub created_at: DateTime<Utc>,     // 创建时间
    pub updated_at: DateTime<Utc>,     // 最后访问/更新时间
    pub active: bool,                  // 是否活跃（软删除标记）
    pub superseded_by: Option<String>, // 被哪条记忆取代
    pub confidence: f32,               // 置信度（0.0~1.0）
}
```

### 4.3 MemoryCategory — 记忆分类

| 分类 | 含义 | 搜索加分 | 半衰期 |
|------|------|---------|--------|
| `Correction` | 纠正信息 | 50.0（最高） | 365 天（最持久） |
| `Preference` | 用户偏好 | 30.0 | 180 天 |
| `Pattern` | 模式/最佳实践 | 25.0 | 90 天 |
| `Fact` | 事实观察 | 20.0 | 60 天 |
| `Custom(String)` | 自定义 | 5.0 | 45 天 |

设计意图：**纠正 > 偏好 > 模式 > 事实**，这符合人类认知直觉——犯过的错不应该再犯，用户的偏好应该最被重视。

### 4.4 TrustLevel — 信任等级

| 等级 | 来源 | 搜索乘数 |
|------|------|---------|
| `High` | 用户明确陈述 | 1.5× |
| `Medium` | 从对话模式中观察 | 1.0× |
| `Low` | LLM 推断 | 0.7× |

### 4.5 ExtractedMemory — LLM 提取结果

```rust
pub struct ExtractedMemory {
    pub category: String,   // 分类字符串 → 通过 from_str_loose() 转换
    pub content: String,
    pub tags: Vec<String>,
    pub confidence: f32,
}
```

LLM 输出 JSON 数组 → `parse_extracted()` 解析 → `store_extracted()` 存入归档。

### 4.6 RecallEntry — 召回记忆 (Layer 3)

```rust
pub struct RecallEntry {
    pub content: String,
    pub source_index: usize,
    pub extracted_at: DateTime<Utc>,
}
```

**当前状态**：结构体已定义，但在代码中**未见实际使用**。自动提取结果直接存入 Layer 2。

---

## 5. 存储机制

### 5.1 文件布局

```
~/.flint/memory/
├── core.json              ← Vec<CoreBlock>，纯 JSON 数组
├── global.json            ← 全局归档记忆（用户级）
└── projects/
    └── {hash}.json        ← 项目级归档记忆（路径哈希命名）
```

项目路径 → `DefaultHasher` → 16 位十六进制哈希作为文件名。

### 5.2 磁盘格式

归档记忆 JSON 文件包含元数据头：

```json
{
  "meta": {
    "version": 1,
    "scope": "project",
    "created_at": "2025-01-01T00:00:00Z",
    "updated_at": "2025-06-01T12:00:00Z",
    "entry_count": 42
  },
  "entries": [
    {
      "id": "mem_550e8400-e29b-...",
      "category": "fact",
      "content": "Rust uses cargo for build management",
      "tags": ["rust", "cargo"],
      "scope": "project",
      "trust": "medium",
      "access_count": 3,
      "created_at": "...",
      "updated_at": "...",
      "active": true,
      "superseded_by": null,
      "confidence": 0.8
    }
  ]
}
```

### 5.3 持久化策略

- **即时写入**：每次 `remember`、`forget`、`update_core` 后立即 `save()` 到磁盘
- **全量序列化**：每次写入都完整序列化整个 store（`serde_json::to_string_pretty`）
- **版本预留**：`StoreMeta.version` 字段为未来数据迁移预留空间
- **零外部依赖**：纯文件 JSON，无数据库

---

## 6. 搜索引擎

### 6.1 评分公式

```
final_score = tf_idf × (1 + tag_bonus) × trust_mult × recency × access_boost × (1 + category_bonus) × confidence
```

| 因子 | 计算方式 | 作用 |
|------|---------|------|
| **TF-IDF** | 查询词在记忆中的出现 × IDF 权重 | 核心相关性 |
| **Tag Bonus** | 每匹配一个标签 +20% | 精确匹配奖励 |
| **Trust Multiplier** | High=1.5, Medium=1.0, Low=0.7 | 来源可信度 |
| **Recency** | `1 / (1 + age_hours / 168)` （1周半衰期） | 新近记忆优先 |
| **Access Boost** | `1 + ln(access_count + 1) × 0.3` | 频繁使用的记忆加权 |
| **Category Bonus** | Correction=1.0, Preference=0.6, Pattern=0.5, Fact=0.4 | 类别重要性 |
| **Confidence** | 指数衰减 + 访问提升 | 整体可靠性 |

### 6.2 文本处理

```rust
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2 && !is_stopword(w))
        .map(|w| w.to_string())
        .collect()
}
```

- 小写化 + 按非字母数字字符分词
- 过滤长度 < 2 的词和约 120 个英文停用词

### 6.3 去重机制

`MemoryStore::find_similar()` 使用 **Jaccard 相似度**（词集交集/并集比）检测重复。阈值默认 0.7。检测到重复时不创建新条目，而是强化已有条目：

```rust
entry.touch();                           // access_count += 1, updated_at = now
entry.confidence = (entry.confidence + 0.1).min(1.0);  // 置信度提升
```

### 6.4 置信度衰减模型

```rust
pub fn effective_confidence(&self) -> f32 {
    let age_hours = (Utc::now() - self.updated_at).num_hours().max(0) as f32;
    let half_life_hours = self.category.half_life_days() as f32 * 24.0;
    let decay = 2.0_f32.powf(-age_hours / half_life_hours);
    let access_boost = (self.access_count as f32 + 1.0).ln() * 0.1;
    (self.confidence * decay + access_boost).min(1.0)
}
```

**指数衰减模型**，模拟人类"常用记忆更清晰、久远记忆逐渐模糊"的特性。

---

## 7. 数据流与生命周期

### 7.1 记忆注入流程（每轮对话）

```
用户输入
  ↓
MemoryManager::format_relevant_memories(&input)
  ↓
search(query, scope=None, limit=5)  ← 跨 global + project 两个 store 搜索
  ↓
格式化为 "[Relevant Memories]" 文本
  ↓
拼接到 effective_system prompt 末尾
  ↓
随 system prompt 发送给 LLM
```

**System Prompt 结构：**
```
├── DEFAULT_SYSTEM（角色、原则、工具说明）
├── Environment（OS、Shell、Working Directory）
├── Skills（可用技能列表）
├── Memory (Core)（核心记忆块 — Layer 1，始终存在）
└── [Relevant Memories]（相关归档记忆 — Layer 2，动态注入）
```

### 7.2 自动提取流程（每轮结束后）

```
对话完成
  ↓
auto_extract_memories() 检查 config.features.memory.auto_extract
  ↓
跳过过短的对话（user_msg < 20 字符 或 assistant_msg < 20 字符）
  ↓
构造 extraction_prompt（要求 LLM 输出 JSON 数组）
  ↓
用临时 Session + 独立 system prompt 调用 LLM
  ↓
parse_extracted() 解析 JSON
  ↓
store_extracted() 存入归档（带 Jaccard 去重）
  ↓
日志 "memory: extracted N fact(s)"
```

### 7.3 Agent 主动记忆管理（Tool Calls）

Agent 通过 5 个工具直接操作记忆：

```
LLM 决策 → tool_call → MemoryManager API → 磁盘持久化
```

### 7.4 记忆生命周期状态

```
创建 → 活跃(active=true) → [访问/强化] → 衰减(confidence↓) → [取代/删除]
                                    ↓                        ↓
                              touch() 更新              active=false (软删除)
                           (access_count++,              superseded_by = new_id
                            updated_at=now)
```

---

## 8. 与系统其他组件的交互

### 8.1 初始化链

```
main.rs
  → MemoryManager::new(config, working_dir)
    → CoreMemory::load_or_create(~/.flint/memory/core.json)
    → MemoryStore::load_or_create(~/.flint/memory/global.json)
    → MemoryStore::load_or_create(~/.flint/memory/projects/{hash}.json)
  → prompt::append_core_memory(&system, mm.core_blocks())
  → register_memory_tools(&mut registry, Arc::new(Mutex::new(mm)))
  → repl::run(..., memory, ...)
```

### 8.2 共享机制

```rust
type SharedMemory = Arc<Mutex<MemoryManager>>;
```

通过 `Arc<Mutex<MemoryManager>>` 在主程序、工具系统、REPL 之间共享同一实例。

### 8.3 暴露的 5 个 Tool

| Tool 名 | 功能 | 操作层 | 对应 Manager 方法 |
|---------|------|--------|------------------|
| `memory_update_core` | 更新核心记忆块 | Layer 1 | `update_core()` |
| `memory_remember` | 保存新记忆到归档 | Layer 2 | `remember()` |
| `memory_forget` | 按 ID 删除记忆 | Layer 2 | `forget()` |
| `memory_search` | 按关键词搜索 | Layer 2 | `search()` |
| `memory_list` | 列出所有记忆 | Layer 2 | `list()` |

### 8.4 REPL 集成节点

```rust
// repl/mod.rs 关键集成点：

// 1. 启动时显示记忆统计
let (core, project, global) = mm.counts();
println!("Memory: {} core blocks, {} project, {} global", ...);

// 2. 每轮前注入相关记忆
if let Some(relevant) = mm.format_relevant_memories(&input) {
    effective_system.push_str(&format!("\n\n{}", relevant));
}

// 3. 每轮后自动提取
if config.features.memory.auto_extract {
    auto_extract_memories(mem, &session, pre_turn_msg_count, ...).await;
}
```

### 8.5 Slash 命令集成

`/memory` 命令通过 `slash.rs` 提供记忆管理界面，可查看统计和管理记忆。

---

## 9. 配置参数

```rust
pub struct MemoryConfig {
    pub max_core_blocks: usize,    // 默认 8，最大核心记忆块数
    pub max_block_chars: usize,    // 默认 2000，每块字符上��
    pub auto_extract: bool,        // 默认 true，自动提取开关
    pub search_limit: usize,       // 默认 5，每轮注入最大记忆条数
    pub dedup_threshold: f64,      // 默认 0.7，去重 Jaccard 阈值
}
```

---

## 10. 设计亮点

1. **三层认知分离**：核心记忆（始终可见）、归档记忆（按需检索）、召回记忆（会话临时）各司其职，模拟人类工作记忆/长期记忆/短期记忆。

2. **零外部依赖**：纯 JSON 文件存储，无需 Redis/SQLite/向量数据库，适合个人开发工具场景，单文件即可备份全部记忆。

3. **置信度衰减 + 访问强化**：指数衰减模型（不同类别不同半衰期）+ 访问频率提升，精确模拟人类记忆的遗忘曲线。

4. **智能去重**：Jaccard 相似度检测避免重复存储，发现相似记忆时强化已有条目而非创建新条目。

5. **类别差异化**：Correction 365 天半衰期 + 50 分搜索加分 vs Fact 60 天 + 20 分，让"不应该再犯的错"始终高优先级。

6. **双作用域隔离**：Global（用户偏好跨项目共享）+ Project（项目知识不互相污染），通过路径哈希自动隔离。

7. **自动提取**：每轮对话后静默调用 LLM 提取有价值信息，使用独立的临时 Session 和精简 system prompt（"You are a memory extraction system"），不影响主对话。

8. **可升级架构**：`search.rs` 模块注释明确说明"designed to be upgradeable: swap in vector search behind the same API later"，接口与实现分离良好。

---

## 11. 已知局限与改进方向

| 局限 | 说明 | 建议改进 |
|------|------|---------|
| **Layer 3 未实现** | `RecallEntry` 已定义但未使用，提取结果直接存入 Layer 2 | 实现真正的会话级临时记忆缓冲 |
| **仅支持英文搜索** | 停用词表和分词器基于英文，CJK 文本会被整段当单 token | 引入 n-gram 或简化的中文分词 |
| **全量序列化** | 每次写入序列化整个 store，条目数千条时可能产生延迟 | 增量写入或 SQLite 后端 |
| **搜索精度有限** | 纯关键词匹配在语义相近但用词不同时效果不佳 | 替换为向量嵌入搜索（接口已预留） |
| **软删除无 GC** | `active=false` 的条目永远留在 JSON 中，文件会膨胀 | 定期清理或压缩机制 |
| **置信度衰减无清理** | 衰减到 0 的记忆不会被自动清理 | 增加定期清理低于阈值的记忆 |
| **每轮提取有 API 成本** | 每轮对话后都调用 LLM 提取 | 仅在对话包含明显值得记忆的内容时提取；使用更小的模型 |
| **Core Block 限制粗糙** | 仅检查字符数，无 token 估算 | 增加 token 估算以适配 API 计费 |

---

## 12. 关键文件索引

| 文件 | 职责 | 行数规模 |
|------|------|---------|
| `flint-memory/src/lib.rs` | 模块入口，架构文档，re-export | ~25 行 |
| `flint-memory/src/types.rs` | 全部核心数据结构 | ~300 行 |
| `flint-memory/src/core.rs` | Layer 1 CoreMemory 管理 | ~150 行 |
| `flint-memory/src/store.rs` | Layer 2 持久化 + 路径管理 | ~190 行 |
| `flint-memory/src/search.rs` | TF-IDF 搜索引擎 | ~170 行 |
| `flint-memory/src/manager.rs` | MemoryManager 统一入口 | ~250 行 |
| `flint-cli/src/tools.rs` | 5 个 Memory Tool 实现 | ~600 行（含基础工具） |
| `flint-cli/src/prompt.rs` | System Prompt 构建 | ~130 行 |
| `flint-cli/src/repl/mod.rs` | REPL 主循环集成 | ~600 行 |

---

## 13. 总结

Flint Memory 系统通过精心设计的三层架构、置信度衰减模型、智能去重和自动提取机制，为 Agent 提供了**类人的记忆能力**。它不仅解决了 LLM 无状态的根本限制，更通过 Correction > Preference > Fact 的优先级设计，让 Agent 能够真正"学习"和"进化"。整个系统以极低的外部依赖（纯 JSON 文件）实现了相当复杂的记忆管理功能，体现了"够用就好"的工程务实精神，同时通过可升级的接口设计为未来的向量搜索等高级功能保留了演进空间。
