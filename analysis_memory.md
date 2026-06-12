# Flint Memory 系统架构分析报告

## 1. 整体架构与设计目标

### 1.1 设计灵感

Flint 的 Memory 系统受 **Letta/MemGPT** 启发，采用**三层记忆架构**，模拟人类的短期/长期记忆机制，让 Agent 能够在多轮对话中积累、检索和运用知识。

### 1.2 三层架构概览

```
┌──────────────────────────────────────────────────────────────┐
│                    System Prompt (每轮注入)                     │
│  ┌────────────────────────────────────────────────────────┐  │
│  │  Layer 1: Core Memory (核心记忆)                        │  │
│  │  - persona / user / project 三个 Block                  │  │
│  │  - 始终在 system prompt 中可见                           │  │
│  │  - Agent 可通过 tool call 读写                           │  │
│  └────────────────────────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────────┤
│  ┌────────────────────────────────────────────────────────┐  │
│  │  Layer 2: Archival Memory (归档记忆)                     │  │
│  │  - 长期持久化存储（JSON 文件）                             │  │
│  │  - 按关键词 TF-IDF 检索                                  │  │
│  │  - 两种作用域：global / project                          │  │
│  └────────────────────────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────────┤
│  ┌────────────────────────────────────────────────────────┐  │
│  │  Layer 3: Recall Memory (召回记忆)                       │  │
│  │  - 会话内临时提取的事实                                    │  │
│  │  - 仅存在于内存中，不跨会话持久化                           │  │
│  └────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘
```

### 1.3 设计目标

| 目标 | 实现方式 |
|------|---------|
| 零外部依赖 | 纯文件 JSON 存储，无需数据库 |
| 可自进化 | Agent 通过 tool call 自主管理记忆 |
| 上下文高效 | 仅核心记忆每轮注入，归档记忆按需检索 |
| 作用域隔离 | 全局记忆（用户级）与项目记忆分离 |
| 置信度衰减 | 记忆随时间衰减，频繁使用的记忆保持高分 |

---

## 2. 核心数据类型与结构

### 2.1 CoreBlock — 核心记忆块 (Layer 1)

```rust
pub struct CoreBlock {
    pub label: String,          // 块标识："persona"、"user"、"project"
    pub content: String,        // 文本内容
    pub limit: usize,           // 字符上限（默认 2000）
    pub read_only: bool,        // 是否只读
    pub updated_at: DateTime<Utc>, // 最后更新时间
}
```

**默认三个 Block：**
- `persona`：Agent 的身份和行为准则
- `user`：用户偏好信息
- `project`：当前项目上下文

### 2.2 MemoryEntry — 归档记忆条目 (Layer 2)

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

### 2.3 MemoryCategory — 记忆分类

| 分类 | 含义 | 搜索加分 | 半衰期（天） |
|------|------|---------|------------|
| `Fact` | 事实观察 | 20.0 | 60 |
| `Preference` | 用户偏好 | 30.0 | 180 |
| `Correction` | 纠正信息 | 50.0 | 365 |
| `Pattern` | 学到的模式/最佳实践 | 25.0 | 90 |
| `Custom(String)` | 自定义类别 | 5.0 | 45 |

### 2.4 TrustLevel — 信任等级

| 等级 | 来源 | 搜索乘数 |
|------|------|---------|
| `High` | 用户明确陈述 | 1.5x |
| `Medium` | 从对话模式中观察（默认） | 1.0x |
| `Low` | LLM 推断 | 0.7x |

### 2.5 RecallEntry — 召回记忆 (Layer 3)

```rust
pub struct RecallEntry {
    pub content: String,        // 提取的内容
    pub source_index: usize,    // 来源消息索引
    pub extracted_at: DateTime<Utc>, // 提取时间
}
```

> **注意**：`RecallEntry` 在代码中已定义结构体，但在当前实现中尚未被广泛使用——自动提取的结果直接存入 Archival Memory（Layer 2），而非暂存为 Recall Memory。

### 2.6 ExtractedMemory — LLM 提取结果

```rust
pub struct ExtractedMemory {
    pub category: String,       // 分类字符串
    pub content: String,        // 记忆内容
    pub tags: Vec<String>,      // 标签
    pub confidence: f32,        // 置信度
}
```

这是 LLM 在自动提取过程中输出的 JSON 结构，经过解析后转为 `MemoryEntry` 存入归档。

---

## 3. 存储机制

### 3.1 文件布局

```
~/.flint/memory/
├── core.json              ← Core Memory（核心记忆块）
├── global.json            ← 全局归档记忆
└── projects/
    └── {hash}.json        ← 项目级归档记忆
```

- **core.json**：存储 `Vec<CoreBlock>`，纯 JSON 数组
- **global.json**：用户级别的长期记忆
- **projects/{hash}.json**：项目级别的长期记忆，文件名是项目目录路径的 `DefaultHasher` 哈希值（16 位十六进制）

### 3.2 磁盘格式

归档记忆的 JSON 文件包含元数据头：

```json
{
  "meta": {
    "version": 1,
    "scope": "project",
    "created_at": "...",
    "updated_at": "...",
    "entry_count": 42
  },
  "entries": [
    {
      "id": "mem_550e8400-e29b-...",
      "category": "fact",
      "content": "...",
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

### 3.3 持久化策略

- **即时写入**：每次 `remember`、`forget`、`update_core` 操作后立即 `save()` 到磁盘
- **全量序列化**：每次保存都是将整个 store 序列化写入（`serde_json::to_string_pretty`）
- **版本管理**：`StoreMeta.version` 字段为未来的数据迁移预留了空间

---

## 4. 搜索机制

### 4.1 搜索算法

Flint 使用**基于关键词的 TF-IDF 风格搜索**（`search.rs`），无向量嵌入依赖。

**评分公式：**

```
final_score = tf_idf_score × (1 + tag_bonus) × trust_mult × recency × access_boost × (1 + category_bonus) × confidence
```

各因子说明：

| 因子 | 计算方式 | 作用 |
|------|---------|------|
| **TF-IDF** | 查询词在记忆中的出现 × IDF 权重 | 核心相关性 |
| **Tag Bonus** | 每匹配一个标签 +20% | 标签精确匹配奖励 |
| **Trust Multiplier** | High=1.5, Medium=1.0, Low=0.7 | 来源可信度 |
| **Recency** | `1 / (1 + age_hours / 168)` （1 周半衰期） | 新近记忆优先 |
| **Access Boost** | `1 + ln(access_count + 1) × 0.3` | 频繁使用的记忆加权 |
| **Category Bonus** | 按类别预设值 / 50 | Correction > Preference > Pattern > Fact |
| **Confidence** | 经过时间衰减和访问提升的有效置信度 | 整体可靠性 |

### 4.2 文本处理

```rust
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2 && !is_stopword(w))
        .map(|w| w.to_string())
        .collect()
}
```

- 小写化
- 按非字母数字字符分词
- 过滤长度 < 2 的词
- 过滤约 120 个英文停用词

### 4.3 去重机制

`MemoryStore::find_similar()` 使用 **Jaccard 相似度**（基于词集交集/并集比）检测重复记忆。当相似度超过阈值（默认 0.7）时，不创建新条目，而是强化已有条目（`touch()` + `confidence += 0.1`）。

### 4.4 置信度衰减模型

```rust
pub fn effective_confidence(&self) -> f32 {
    let age_hours = (Utc::now() - self.updated_at).num_hours().max(0) as f32;
    let half_life_hours = self.category.half_life_days() as f32 * 24.0;
    let decay = 2.0_f32.powf(-age_hours / half_life_hours);
    let access_boost = (self.access_count as f32 + 1.0).ln() * 0.1;
    (self.confidence * decay + access_boost).min(1.0)
}
```

采用**指数衰减模型**，不同类别的半衰期不同（Fact 60 天，Correction 365 天），频繁访问可提升有效置信度。

---

## 5. 记忆的生命周期管理

### 5.1 创建

```
用户对话 → 自动提取 (LLM) → parse_extracted() → store_extracted() → remember()
                                                                         ↓
                                                              dedup check (Jaccard)
                                                              ↓ 无重复       ↓ 有重复
                                                         创建新条目     强化已有条目
```

Agent 也可以主动调用 `memory_remember` 工具创建记忆。

### 5.2 更新

- **Core Memory**：Agent 调用 `memory_update_core` 工具 → `CoreMemory::update()` → 检查 `read_only` 和 `limit` → 写入并持久化
- **Archival Memory**：搜索时自动 `touch()` 更新 `access_count` 和 `updated_at`

### 5.3 删除

- **软删除**（默认）：`MemoryStore::remove()` 将 `active` 设为 `false`
- **硬删除**：`MemoryStore::hard_remove()` 从数组中物理移除
- **取代**：`MemoryEntry::supersede()` 标记为非活跃并记录取代者 ID

### 5.4 记忆注入流程

每轮对话时：

```
用户消息 input
    ↓
MemoryManager::format_relevant_memories(input)
    ↓
search(query, scope=None, limit=5)
    ↓
格式化为 "[Relevant Memories]" 文本
    ↓
拼接到 effective_system prompt 末尾
    ↓
发送给 LLM
```

### 5.5 自动提取流程

```
对话完成（每轮结束后）
    ↓
auto_extract_memories() 检查是否启用
    ↓
跳过过短的对话（< 20 字符）
    ↓
构造 extraction_prompt（要求 LLM 输出 JSON 数组）
    ↓
用临时 Session 调用 LLM 提取
    ↓
parse_extracted() 解析 JSON
    ↓
store_extracted() 存入归档（带去重）
    ↓
日志输出 "memory: extracted N fact(s)"
```

---

## 6. 与 Agent 系统的集成方式

### 6.1 模块依赖关系

```
flint-cli (主程序)
    ├── flint-agent (Agent 循环)
    │   ├── agent.rs  → run_turn() 主循环
    │   └── tool.rs   → Tool trait, ToolRegistry
    ├── flint-memory (记忆系统)
    │   ├── core.rs    → CoreMemory
    │   ├── store.rs   → MemoryStore (持久化)
    │   ├── search.rs  → 关键词搜索
    │   ├── manager.rs → MemoryManager (统一入口)
    │   └── types.rs   → 数据类型定义
    └── tools.rs (工具注册)
        ├── 5 个 Memory 工具
        └── 7 个基础工具 (read/write/edit/bash/grep/glob/web_fetch)
```

### 6.2 共享机制

```rust
// main.rs 中初始化
let memory: Option<Arc<Mutex<MemoryManager>>> = ...;
let shared = Arc::new(Mutex::new(mm));

// 注册到工具注册表
tools::register_memory_tools(&mut registry, shared.clone());

// 传递给 REPL
repl::run(..., memory, ...);
```

使用 `Arc<Mutex<MemoryManager>>` 在主程序、工具系统、REPL 之间共享同一份记忆管理器实例。

### 6.3 暴露的 5 个工具

| 工具名 | 功能 | 对应层 |
|--------|------|--------|
| `memory_remember` | 保存新记忆到归档 | Layer 2 |
| `memory_forget` | 按 ID 删除记忆 | Layer 2 |
| `memory_search` | 按关键词搜索记忆 | Layer 2 |
| `memory_list` | 列出所有记忆 | Layer 2 |
| `memory_update_core` | 更新核心记忆块 | Layer 1 |

### 6.4 作用域隔离

- **Global**（`~/.flint/memory/global.json`）：用户级别偏好，跨项目共享
- **Project**（`~/.flint/memory/projects/{hash}.json`）：项目特定知识
- 自动提取的记忆默认存入 Project 作用域

---

## 7. Memory 在 System Prompt 中的注入方式

### 7.1 初始化阶段

```rust
// main.rs
let mm = MemoryManager::new(mem_config, Some(working_dir))?;
system = prompt::append_core_memory(&system, mm.core_blocks());
```

`append_core_memory()` 将核心记忆块追加到系统提示末尾：

```
## Memory (Core)
The following is your core memory — always available context.
You can update these blocks using the memory_update_core tool.

[persona]
You are flint, a fast and focused coding agent...

[user]
(User prefers concise answers...)

[project]
(Working on flint-agent, a Rust agent harness...)
```

### 7.2 每轮动态注入

```rust
// repl/mod.rs — 每轮对话前
if let Some(ref mem) = memory {
    let mut mm = mem.lock().unwrap();
    if let Some(relevant) = mm.format_relevant_memories(&input) {
        effective_system.push_str(&format!("\n\n{}", relevant));
    }
}
```

根据用户当前输入，搜索相关归档记忆并追加到 system prompt：

```
[Relevant Memories]
1. [fact][project] Rust uses cargo for build management
2. [preference][global] User prefers TypeScript over JavaScript
```

### 7.3 注入层次总结

```
System Prompt 结构：
├── DEFAULT_SYSTEM（角色、原则、工具说明）
├── Environment（OS、Shell、Working Directory）
├── Skills（可用技能列表）
├── Swarm Mode（如启用）
├── Memory (Core)（核心记忆块 — Layer 1，始终存在）
└── [Relevant Memories]（相关归档记忆 — Layer 2，动态注入）
```

---

## 8. 设计亮点���潜在改进点

### 8.1 设计亮点

1. **三层分离清晰**：核心记忆（始终可见）、归档记忆（按需检索）、召回记忆（会话临时）各司其职，认知负担分配合理。

2. **零外部依赖**：纯 JSON 文件存储，无需 Redis/SQLite/向量数据库，降低部署复杂度，适合个人开发工具场景。

3. **置信度衰减模型**：基于指数衰减 + 访问频率提升的有效置信度计算，模拟了人类"常用记忆更清晰、久远记忆逐渐模糊"的特性。

4. **智能去重**：Jaccard 相似度检测避免重复存储，并通过强化（`touch()` + `confidence += 0.1`）合并重复信息。

5. **自动提取**：每轮对话后静默调用 LLM 提取有价值的信息，无需用户手动管理。

6. **类别差异化**：不同类别的记忆有不同搜索加分和半衰期（Correction 365 天 vs Fact 60 天），符合直觉——纠正信息应比普通事实更持久。

7. **作用域隔离**：Global/Project 双作用域设计，用户偏好跨项目共享，项目知识不会互相污染。

8. **可升级架构**：`search.rs` 明确注释"designed to be upgradeable: swap in vector search behind the same API later"，接口与实现分离良好。

### 8.2 潜在改进点

1. **Layer 3 (Recall Memory) 未充分实现**：`RecallEntry` 类型已定义但代码中未见实际使用场景。当前自动提取结果直接存入 Layer 2，建议实现真正的会话级临时记忆，作为 Layer 1 和 Layer 2 之间的缓冲层。

2. **搜索仅支持英文**：停用词表和分词器基于英文设计（按非字母数字字符分割）。中文等 CJK 语言会整段作为单个 token，搜索效果较差。建议引入 n-gram 或简化的中文分词。

3. **全量序列化性能隐患**：每次写入都将整个 store 序列化为 JSON。当记忆条目数量增长到数千条时，可能产生明显延迟。可考虑增量写入或 SQLite 后端。

4. **搜索算法精度有限**：纯关键词匹配（TF-IDF）在语义相近但用词不同的场景下效果不佳。如代码中所注释，后续可替换为向量嵌入搜索。

5. **硬删除缺乏自动清理**：`active = false` 的软删除条目仍保留在 JSON 文件中，无 GC 机制。长期使用后文件可能膨胀。

6. **并发安全**：`MemoryManager` 使用 `Mutex` 保护，但在 `search()` 方法中先锁后 `touch_entry()` 再保存，期间其他线程可能修改数据。可考虑更细粒度的锁或读写锁。

7. **Core Block 限制机制较粗**：仅检查字符数上限，无 token 数估算。对于使用 token 计费的 API，字符限制与 token 限制可能不一致。

8. **记忆过期机制缺失**：虽然有置信度衰减，但衰减到 0 的记忆不会被自动清理或归档。可增加定期清理低于阈值的记忆。

9. **项目路径哈希碰撞**：使用 `DefaultHasher` 生成 16 位十六进制哈希作为项目文件名，理论上存在碰撞可能（虽然概率极低）。可考虑使用路径的 base64 编码或目录名。

10. **LLM 提取的成本开销**：每轮对话后都调用 LLM 进行记忆提取（如果启用），这对 API 成本有影响。可考虑：
    - 仅在对话包含明显值得记忆的内容时提取
    - 使用更小/更便宜的模型进行提取
    - 批量提取（累积几轮后一次性提取）

---

## 附录：关键文件索引

| 文件 | 职责 |
|------|------|
| `flint-memory/src/lib.rs` | 模块入口，三层架构文档注释，公共类型导出 |
| `flint-memory/src/types.rs` | 所有核心数据结构定义（CoreBlock, MemoryEntry, MemoryCategory 等） |
| `flint-memory/src/core.rs` | Layer 1 CoreMemory 管理（加载、保存、更新、渲染） |
| `flint-memory/src/store.rs` | Layer 2 MemoryStore 持久化（JSON 文件读写、路径管理） |
| `flint-memory/src/search.rs` | TF-IDF 搜索引擎（评分、分词、停用词过滤） |
| `flint-memory/src/manager.rs` | MemoryManager 统一入口（编排三层、去重、LLM 提取） |
| `flint-cli/src/tools.rs` | 5 个 Memory Tool 实现（注册到 ToolRegistry） |
| `flint-cli/src/prompt.rs` | System Prompt 构建（追加 Core Memory 块） |
| `flint-cli/src/main.rs` | 初始化 MemoryManager 并注入系统提示 |
| `flint-cli/src/repl/mod.rs` | REPL 循环中的动态记忆注入与自动提取 |
