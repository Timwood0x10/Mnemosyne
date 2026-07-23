# MCP 工具参考

服务器通过模型上下文协议（MCP）暴露 **6 个 MCP 工具**。每个工具都是一个注册的处理器，在 JSON-RPC 层验证输入模式。

## 工具概览

| 工具 | 描述 | 必需参数 |
|---|---|---|
| `memory_distill` | 运行 8 阶段蒸馏流水线 | `conversation_id`, `messages[]` |
| `memory_compile` | 构建会话状态（可选蒸馏） | `messages[]` |
| `memory_search` | 搜索已存储的记忆 | `query` |
| `memory_store` | 手动写入一条记忆 | `content`, `memory_type` |
| `memory_feedback` | 记录 Agent 对记忆的反馈 | `memory_id`, `rating` |
| `memory_stats` | 获取租户记忆统计 | （无） |

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
            "enum": ["user", "assistant"]
          },
          "content": {
            "type": "string",
            "description": "消息内容（纯文本）"
          }
        },
        "required": ["role", "content"]
      },
      "description": "按时间顺序排列的对话消息数组"
    },
    "tenant_id": {
      "type": "string",
      "description": "可选的租户标识符（覆盖默认值）"
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
          "role": {"type": "string"},
          "content": {"type": "string"},
          "tool_invocations": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "tool_name": {"type": "string"},
                "tool_args": {"type": "string"},
                "result": {"type": "string"},
                "status": {"type": "string"},
                "reasoning": {"type": "string"}
              }
            }
          }
        },
        "required": ["role", "content"]
      }
    },
    "conversation_id": {
      "type": "string",
      "description": "可选——包含此项也会运行蒸馏"
    },
    "tenant_id": {
      "type": "string"
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
      "description": "最大结果数（默认：配置的 retrieval_limit）"
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
      "description": "可选值：knowledge, skill, preference, experience, interaction, profile"
    },
    "importance": {
      "type": "number",
      "description": "重要性评分 0.0-1.0（默认：0.5）"
    },
    "tenant_id": {
      "type": "string"
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
    "rating": {
      "type": "number",
      "description": "评分 0.0-1.0（0 = 无用，1 = 非常有用）"
    },
    "tenant_id": {
      "type": "string"
    }
  },
  "required": ["memory_id", "rating"]
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
