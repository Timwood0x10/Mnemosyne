# MCP 工具参考

> ⚠️ 本文件是**核心流水线工具**的详细参考（历史核心集：`memory_*` / `character_*`）。
> 服务器实际注册的**完整**工具清单以 [`README.zh.md`](../README.zh.md) 的「MCP 工具一览」为准。
> 认知状态（`state_timeline` / `fact_provenance`）、决策（`decision_trace` / `decision_search`）、
> 知识图谱查询（`inspect_entity` / `timeline` / `relation_graph` / `search_graph` /
> `trace_path` / `evidence`）、外部知识（`generalize_compile` / `knowledge_attach` /
> `knowledge_ingest` / `memory_export` / `memory_import`）与人设守卫
> （`persona_check` / `persona_inject` / `relationship_update` / `relationship_query` /
> `persona_timeline` / `story_bridge`）等工具在本文件中不逐一展开。

每个工具都是一个注册的处理器，在 JSON-RPC 层验证输入模式。

## 工具概览

| 工具 | 描述 | 必需参数 |
|---|---|---|
| `memory_distill` | 运行 8 阶段蒸馏流水线 | `conversation_id`, `messages[]` |
| `memory_compile` | 构建会话状态（可选蒸馏） | `messages[]` |
| `memory_search` | 搜索已存储的记忆 | `query` |
| `memory_store` | 手动写入一条记忆 | `content`, `memory_type` |
| `memory_feedback` | 记录 Agent 对记忆的反馈 | `memory_id` |
| `memory_stats` | 获取租户记忆统计 | （无） |
| `character_search` | 按姓名/属性/小说搜索人物 | `query` |
| `character_network` | BFS 遍历人物关系图谱 | `name` |
| `character_ingest` | 从小说语料蒸馏人物图谱 | （无） |
| `character_graph` | 导出 3D 人物图谱 JSON 用于可视化 | （无） |
| `state_timeline` | 还原实体认知状态的演化：分维度区间 + 确定性变迁 | `entity_id`（可选 `tenant_id`） |
| `fact_provenance` | 审计一条事实：置信度 / 认知状态 / 证据 / 推导链 | `fact_id`（可选 `tenant_id`） |
| `decision_trace` | 回溯决策的支持事实（非因果） | `decision_id`（可选 `tenant_id`） |
| `decision_search` | 按关键词检索某主体的决策 | `subject`（可选 `tenant_id`） |

---

## 1. `memory_distill`

主要工具——运行完整的 8 阶段流水线，从对话中提取、分类、评分、过滤、压缩、嵌入、解决冲突并持久化记忆。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "conversation_id": {
      "type": "string",
      "description": "对话会话的唯一标识符"
    },
    "messages": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "role": {
            "type": "string",
            "enum": ["user", "assistant", "system"]
          },
          "content": {
            "type": "string",
            "description": "消息内容（纯文本）"
          },
          "tool_call_id": {
            "type": "string",
            "description": "工具调用标识符（用于工具结果消息）"
          },
          "turn_id": {
            "type": "string",
            "description": "轮次标识符（用于分组消息）"
          }
        },
        "required": ["role", "content"]
      },
      "description": "按时间顺序排列的对话消息数组"
    },
    "tenant_id": {
      "type": "string",
      "description": "可选的租户标识符（覆盖默认值）"
    },
    "user_id": {
      "type": "string",
      "description": "可选的用户标识符"
    }
  },
  "required": ["conversation_id", "messages"]
}
```

### 示例请求

```json
{
  "conversation_id": "session-42",
  "messages": [
    {"role": "user", "content": "如何在 Rust 中解析 JSON？"},
    {"role": "assistant", "content": "使用 serde_json::from_str 配合类型化结构体。"}
  ]
}
```

### 示例响应

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"extracted\":2,\"classified\":2,\"scored\":2,\"filtered\":0,\"compressed\":2,\"embedded\":0,\"conflicts_detected\":0,\"conflicts_replaced\":0,\"stored\":2,\"rejected_low_importance\":0,\"errors\":0}"
    }
  ],
  "is_error": false
}
```

### 行为

- 空 `messages[]` 返回零指标，不报错
- 所有消息必须有非空 `content`
- 未通过噪音/安全过滤器的消息被静默跳过
- 返回一个包含各阶段计数的 JSON 对象

---

## 2. `memory_compile`

分析对话消息以构建结构化的 `SessionState`：目标、模块、文件、问题、决策和推理链。可选地同时运行蒸馏。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "messages": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "role": {"type": "string", "enum": ["user", "assistant", "system"]},
          "content": {"type": "string"}
        },
        "required": ["role", "content"]
      }
    },
    "distill": {
      "type": "boolean",
      "default": false,
      "description": "同时运行蒸馏流水线"
    },
    "conversation_id": {
      "type": "string",
      "description": "distill=true 时必须提供"
    },
    "tenant_id": {
      "type": "string",
      "default": "default"
    },
    "user_id": {
      "type": "string"
    },
    "agent_id": {
      "type": "string",
      "description": "可选：Agent 身份，启用后会把 Agent 自己的承诺编译成 Decision"
    },
    "decision_outcomes": {
      "type": "array",
      "description": "可选：声明早先的承诺后来怎么样了，如 [{\"decision_id\":3,\"outcome\":\"fulfilled\"}]。不从对话里推断任何东西；同一个决策的**首次**结果生效，之后的声明只回显不覆盖；未知 id 以 missing 回报而不是让本次调用失败。",
      "items": {
        "type": "object",
        "properties": {
          "decision_id": {"type": "integer", "minimum": 1},
          "outcome": {"type": "string", "enum": ["fulfilled", "violated"]}
        },
        "required": ["decision_id", "outcome"]
      }
    }
  },
  "required": ["messages"]
}
```

### 示例响应

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"session\":{\"current_goal\":\"优化SQLite查询性能\",\"current_module\":\"store\",\"current_files\":[\"src/store.rs\"],\"open_problems\":[],\"reasoning_chain\":[{\"trigger\":\"用户询问索引\",\"tool_name\":\"analyze\",\"tool_args\":\"EXPLAIN QUERY PLAN\",\"status\":\"success\",\"reasoning\":\"在 (user_id, created_at) 上添加了复合索引\"}]},\"knowledge\":[],\"decisions\":[{\"decision\":\"切换到 WAL 模式\",\"rationale\":\"更好的并发读取性能\",\"module\":\"store\",\"importance\":0.8}]}"
    }
  ],
  "is_error": false
}
```

### 检测规则

| 组件 | 检测方式 |
|---|---|
| `current_goal` | 第一条用户消息 |
| `current_module` | 从消息中文件路径识别的模块名 |
| `current_files` | 匹配 `src/...` 模式的行 |
| `open_problems` | 没有收到助手回应的用户问题 |
| `knowledge` | 提取的问题-解决方案对 |
| `decisions` | 包含 `DONE`、`DECIDED`、`CHOSEN` 或 `OPTED` 的行 |
| `reasoning_chain` | 助手消息上的 `tool_invocations` 数组 |

---

## 3. `memory_search`

使用配置的检索模式（关键词、向量或混合）搜索已存储的记忆。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "搜索查询文本"
    },
    "limit": {
      "type": "integer",
      "default": 5,
      "description": "最大结果数（默认：5）"
    },
    "memory_type": {
      "type": "string",
      "description": "可选的过滤：knowledge, skill, preference, experience, interaction, profile"
    },
    "tenant_id": {
      "type": "string"
    }
  },
  "required": ["query"]
}
```

### 示例响应

```json
{
  "content": [
    {
      "type": "text",
      "text": "[{\"id\":\"a1b2c3d4\",\"content\":\"问题：如何优化SQLite查询性能\\n解决方案：添加索引、使用EXPLAIN QUERY PLAN、考虑WAL模式\",\"memory_type\":\"knowledge\",\"importance\":0.85,\"score\":0.73,\"created_at\":\"2026-07-23T10:30:00Z\"}]"
    }
  ],
  "is_error": false
}
```

### 按模式的评分

| 模式 | 评分组成 |
|---|---|
| `keyword` | 0.7 × BM25 + 0.3 × importance |
| `vector` | 余弦相似度 |
| `hybrid` | 0.6 × 语义 + 0.2 × 关键词(BM25) + 0.2 × importance |

---

## 4. `memory_store`

手动写入一条记忆到存储中，无需运行完整的蒸馏流水线。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "content": {
      "type": "string",
      "description": "记忆内容（建议使用 '问题：解决方案' 格式）"
    },
    "memory_type": {
      "type": "string",
      "enum": ["knowledge", "preference", "skill", "experience", "interaction", "profile"],
      "default": "knowledge",
      "description": "可选值：knowledge, skill, preference, experience, interaction, profile"
    },
    "confidence": {
      "type": "number",
      "default": 0.5,
      "description": "置信度评分 0.0-1.0（默认：0.5）"
    },
    "tenant_id": {
      "type": "string",
      "default": "default"
    }
  },
  "required": ["content", "memory_type"]
}
```

### 示例响应

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"id\":\"x1y2z3w4\",\"stored\":true}"
    }
  ],
  "is_error": false
}
```

---

## 5. `memory_feedback`

记录用户/Agent 对已存储记忆的反馈。可用于强化学习或手动整理。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "memory_id": {
      "type": "string",
      "description": "要提供反馈的记忆 ID"
    },
    "useful": {
      "type": "boolean",
      "default": true,
      "description": "记忆是否有用（true = 有用，false = 无用）"
    },
    "tenant_id": {
      "type": "string",
      "default": "default"
    }
  },
  "required": ["memory_id"]
}
```

### 示例响应

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"memory_id\":\"a1b2c3d4\",\"recorded\":true}"
    }
  ],
  "is_error": false
}
```

---

## 6. `memory_stats`

获取租户的记忆统计信息，不暴露实际记忆内容。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "tenant_id": {
      "type": "string"
    }
  }
}
```

### 示例响应

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"total\":142,\"by_type\":{\"knowledge\":58,\"skill\":12,\"preference\":23,\"experience\":19,\"interaction\":22,\"profile\":8},\"total_tenant_memories\":142}"
    }
  ],
  "is_error": false
}
```

---

## 7. `character_search`

按姓名、别名、外貌、性格或描述搜索人物知识图谱。可选包含每个人物的事件和关系。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "query": {"type": "string", "description": "搜索查询 – 匹配姓名、外貌、性格、描述"},
    "tenant_id": {"type": "string", "default": "novels"},
    "novel": {"type": "string", "description": "可选小说过滤（如 水浒传、西游记、三国演义、红楼梦）"},
    "limit": {"type": "integer", "default": 10},
    "include_events": {"type": "boolean", "default": false, "description": "是否包含每个人物的事件"},
    "include_relations": {"type": "boolean", "default": false, "description": "是否包含每个人物的关系"}
  },
  "required": ["query"]
}
```

### 响应示例

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"results\":[{\"id\":\"...\",\"name\":\"扈三娘\",\"novel\":\"水浒传\",\"aliases\":[\"一丈青\"],\"clothing\":\"...\",\"personality\":\"...\",\"description\":\"登场水浒传共12回\",\"importance\":0.4}],\"stats\":{\"total_characters\":92,\"total_events\":340,\"total_relations\":128}}"
    }
  ],
  "is_error": false
}
```

---

## 8. `character_network`

通过 BFS 遍历人物关系图谱，从指定人物开始向外扩展最多 `depth` 跳。返回递归的人物树，包含其事件、关系和连接。

响应中的每个 `CharacterRelation` 都携带由 `character_ingest` 填充的完整维度评分 `metadata`（`co_occurrence_score`、`event_coupling_score`、`relation_type_score` 等），调用方可以直接对边排序或过滤而无需重新运行流水线。维度明细见 [§10 `character_graph`](#10-character_graph)。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "name": {"type": "string", "description": "起始人物姓名"},
    "tenant_id": {"type": "string", "default": "novels"},
    "novel": {"type": "string", "description": "可选小说过滤"},
    "depth": {"type": "integer", "default": 2, "description": "遍历深度（1-5，默认 2）"}
  },
  "required": ["name"]
}
```

---

## 9. `character_ingest`

对四大名著（水浒传、三国演义、红楼梦、西游记）运行完整的人物蒸馏流水线。从语料目录读取 `.txt` 文件，按回目切分，提取人物登场/事件/描写，通过关键词邻近检测关系，并将所有数据持久化到人物存储中。

**这是一个重型操作** — 完整语料（约 8MB 文本）预计需要 30-60 秒。运行一次填充存储后，使用 `character_search` / `character_network` / `character_graph` 进行查询。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "corpus_dir": {"type": "string", "default": "corpus", "description": "包含小说 .txt 文件的目录路径"}
  }
}
```

### 响应示例

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"status\":\"completed\",\"characters\":179,\"events\":520,\"relations\":210}"
    }
  ],
  "is_error": false
}
```

### 提取流水线

| 阶段 | 描述 |
|---|---|
| 回目切分 | 解析 `第N回` 标记，中文数字 → 整数 |
| 别名匹配 | 按长度优先解决重叠，覆盖所有人物别名 |
| 事件提取 | 人物名附近含强动词（杀/斩/擒/战...）的句子 |
| 描写提取 | 外貌关键词（头戴/身穿/铠甲...）+ 性格关键词（相貌/性格/勇猛...） |
| 死亡检测 | 人物名 100 字内出现死亡关键词（死/亡/卒/阵亡...） |
| 共现统计 | 同一回目中出现的人物 → 关系权重 |
| 关系分类 | 关键词邻近（夫妻/结义/父子/师徒/君臣/仇敌/挚友/姐妹/亲戚），回退 "关联"。流水线会在两个人物共现的**每一回**重新评估关系类型，一旦检测到具体类型（非"关联"）即锁定，因此第一章的结义誓言不会因为前面章节的普通相遇场景而被覆盖。 |
| 维度评分 | 每条关系在 `metadata` 中存储三个独立分数：`co_occurrence_score`（共现频次 / 15）、`event_coupling_score`（共享事件数 / 总事件数）、`relation_type_score`（具体类型 1.0，"关联" 0.3）。综合 `importance` = `0.5·共现 + 0.3·事件 + 0.2·类型`，限制在 `[0, 1]`。 |
| 重要度 | 人物：`min(1.0, 回数/30)`。关系：综合维度评分（见上）。 |

---

## 10. `character_graph`

导出完整的 3D 人物关系图谱为结构化 JSON，用于前端可视化。返回**节点**（含多维属性的人物）和**边**（带权重的关系）。

这是"立体人物关系网络"：每个节点携带外貌（`clothing`）、心理（`personality`）和复合描述维度；每条边携带关系类型和维度权重评分。图谱支持 **人物 → 事件 → 关联人物** 模式，关系之间有维度评分。

### 输入模式

```json
{
  "type": "object",
  "properties": {
    "tenant_id": {"type": "string", "default": "novels"},
    "novel": {"type": "string", "description": "可选小说过滤（如 水浒传）"},
    "max_nodes": {"type": "integer", "default": 200, "description": "返回的最大节点数"}
  }
}
```

### 响应示例

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"nodes\":[{\"id\":\"宋江\",\"label\":\"宋江\",\"novel\":\"水浒传\",\"aliases\":[\"及时雨\",...],\"dimensions\":{\"appearance\":\"头戴...\",\"personality\":\"仗义...\",\"description\":\"登场水浒传共80回\"},\"importance\":1.0}],\"edges\":[{\"source\":\"宋江\",\"target\":\"吴用\",\"relation_type\":\"结义\",\"weight\":0.73,\"description\":\"共现12章\",\"chapter\":1,\"dimensions\":{\"co_occurrence_score\":0.8,\"event_coupling_score\":0.5,\"relation_type_score\":1.0,\"co_occurrence_count\":12,\"shared_event_count\":3,\"detected_at_chapter\":1}}],\"stats\":{\"total_characters\":179,\"total_events\":520,\"total_relations\":210,\"visible_nodes\":179,\"visible_edges\":210}}"
    }
  ],
  "is_error": false
}
```

### 图谱结构

```
节点维度：
  ├── appearance   (clothing / 外貌)
  ├── personality  (心理 / 性格)
  └── description  (复合摘要)

边维度：
  ├── relation_type        (夫妻/结义/父子/师徒/君臣/仇敌/挚友/姐妹/亲戚/关联)
  ├── weight               (综合 importance = 0.5·共现 + 0.3·事件 + 0.2·类型)
  ├── chapter              (检测到关系类型的回目，时序锚点)
  └── dimensions           (每条边的维度评分明细)
      ├── co_occurrence_score  (共现次数 / 15，基于频次的强度)
      ├── event_coupling_score (共享事件 / 总事件，语义耦合度)
      ├── relation_type_score  (具体类型 1.0，"关联" 0.3)
      ├── co_occurrence_count   (原始共现回目数)
      ├── shared_event_count    (双方均被提及的事件数)
      └── detected_at_chapter   (匹配到类型关键词的回目)
```

---

## 错误处理

所有工具都以一致的格式返回错误：

```json
{
  "content": [
    {
      "type": "text",
      "text": "错误描述"
    }
  ],
  "is_error": true
}
```

常见错误类别：

| 类别 | 示例 |
|---|---|
| 无效输入 | `"invalid input: missing field 'messages'"` |
| 未找到 | `"not found: memory 'abc123' does not exist"` |
| 存储 | `"storage error: sqlite error: database is locked"` |
| 嵌入 | `"embedding service error: transport error: connection refused"` |
| 蒸馏 | `"distillation error at phase 'extract': no messages"` |

---

## 11. `state_timeline`

把实体的认知状态演化投影为**分维度状态区间**与**确定性变迁**。只读、ADD-only：
事实永不被删除，状态永远可由事实重算。

### 输入模式

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `entity_id` | integer | 是 | 要还原状态史的实体 id |
| `dimension` | string | 否 | 限定单个维度：`goal` / `preference` / `emotion` / `relationship` / `identity` |
| `tenant_id` | string | 否 | 该实体必须属于的租户；给定时跨租户 id 会返回 not found |

### 示例请求

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call",
 "params":{"name":"state_timeline","arguments":{"entity_id":1,"dimension":"preference"}}}
```

### 示例响应

```json
{"entity_id":1,"dimensions":[{
  "dimension":"preference",
  "intervals":[
    {"from":2024,"to":2025,"value":{"content":"喜欢 Python"},"fact_ids":[1],"evidence_ids":[]},
    {"from":2025,"to":null,"value":{"content":"开始喜欢 Rust"},"fact_ids":[2],"evidence_ids":[]}
  ],
  "transitions":[
    {"from_index":0,"to_index":1,"at":2025,"transition_type":"gradual_change","evidence_ids":[]}
  ]}]}
```

### 行为

- 维度按 `FactType` 选取，与当前状态口径（`cognitive_context`）一致；不是按 payload 字段选取。
- 区间值是**状态生效时间**（`from`/`to`），不是观察时间；`to: null` 表示状态仍然成立。
- 每个区间的 `evidence_ids` 指向 `evidence` 表中承载该状态的**原文**：编译期会把
  `payload.evidence.text` 登记成证据行（同一条话语产生的多条事实共享一行），
  因此"每个状态各自带证据"在真实数据上成立。
- `transitions` **允许为空**：没有确定信号时只给区间，绝不臆造变迁。三种信号按强度排序：
  `stance_flip`（同话题否定翻转）> `behavioral_confirmation`（先说意向、后见行动）> `gradual_change`（同话题内取值变化）。
- 折叠值取该维度 `value_keys` 中第一个字符串字段（历史层以 `content` 优先），全部缺失时才回退完整 payload；
  同一维度的当前状态视图用的是同一张表的 `topic_keys`（以话题优先，保留"每个话题的最新一条"语义）。
- 折叠键只用**稳定**字段：像 companion 主题的 `occurrences` 这类每次编译都会增长的计数器，
  一旦进入折叠键就会把同一个状态切成 N 个区间，因此不纳入。

---

## 12. `fact_provenance`

审计一条事实**为什么成立**。

### 输入模式

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `fact_id` | integer | 是 | 要审计的事实 id（≥1） |
| `tenant_id` | string | 否 | 该事实所属实体必须属于的租户；给定时跨租户 id 会返回 not found |

### 示例响应

```json
{"fact_id":12,"fact_type":"Preference","time":2026,
 "payload":{"content":"喜欢 Rust"},
 "confidence":0.85,"status":"active",
 "evidence":"2026-08-15: “我从去年开始喜欢 Rust”",
 "derived_from":[{"fact_id":11,"fact_type":"Preference","time":2024,"content":"喜欢 Python","status":"active"}]}
```

### 行为

- `status` 为三态：`active` / `superseded` / `contradicted`，与 `confidence`、衰减三者正交。
- `derived_from` 是**推导链**（`F2 derived_from F1` 表示 F2 由 F1 推得），**不是因果**。
- 链式展开有深度/广度上限；损坏的链以 `{"fact_id":N,"missing":true}` 报告，而不是整体失败。
- `evidence` 为 `null` 表示该事实没有原文证据锚点。编译产生的事实现在都带锚点
  （写入时自动登记 `evidence` 行），所以这个字段对真实数据是**有值**的。
- 只读工具，永不写入。

---

## 13. `decision_trace`

把一个决策回溯到支持它的事实（supporting evidence，**不是因果**）。

### 输入模式

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `decision_id` | integer | 是 | 决策 id（≥1） |
| `tenant_id` | string | 否 | 该决策主体必须属于的租户；给定时跨租户 id 会返回 not found |

### 示例响应

```json
{"decision_id":1,"subject":1,"verb":"promise",
 "object":"我答应你明天陪你去医院","made_at":1780000000,
 "because":[{"fact_id":9,"fact_type":"Event","content":"我答应你明天陪你去医院","status":"active"}],
 "outcome":null,"status":"open"}
```

### 行为

- 决策由**编译**产生，不新增工具：`memory_compile` 把明确承诺
  （`答应/承诺/保证/发誓` 或 `promise/i'll` 等）编译为 `Decision`，并先落一条承载该话语的
  Event 事实作为锚点，`because` 指向它。
- 单独的「我会…」是**计划**（编译为 Goal 事实），不算承诺。
- `outcome` 创建时为空、`status` 为 `open`；记录结果后 `status` 变为 `closed`，
  且**首次记录的结果不可被覆盖**。

---

## 14. `decision_search`

按关键词检索某个主体（subject）的决策。

### 输入模式

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `subject` | integer | 是 | 决策主体的实体 id |
| `keyword` | string | 否 | 在 `verb`/`object` 上做大小写不敏感匹配；空串表示不过滤 |
| `limit` | integer | 否 | 返回条数上限，默认 10，最大 100（超出会被钳制） |
| `tenant_id` | string | 否 | 该主体必须属于的租户；给定时跨租户 id 会返回 not found |

### 示例响应

```json
{"decisions":[{"decision_id":1,"verb":"promise",
 "object":"我答应你明天陪你去医院","made_at":1780000000,
 "outcome":null,"status":"open","because":[{"fact_id":9,"..."}]}]}
```

### 行为

- 结果按 `made_at` 倒序（最新优先）。
- `keyword` 中的 `%` / `_` / `\` 按**字面量**匹配，不作为 LIKE 通配符。
- 给 `tenant_id` 时，`subject` 归属不符返回 not found（不暴露"该 id 存在"这一信息），
  与 `memory_decay` 的租户隔离口径一致。
