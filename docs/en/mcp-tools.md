# MCP Tools Reference

The server exposes **6 MCP tools** via the Model Context Protocol. Each tool is a registered handler with an input schema validated at the JSON-RPC layer.

## Tool Overview

| Tool | Description | Required Arguments |
|---|---|---|
| `memory_distill` | Run the 8-stage distillation pipeline | `conversation_id`, `messages[]` |
| `memory_compile` | Build session state (optionally distill) | `messages[]` |
| `memory_search` | Search stored memories | `query` |
| `memory_store` | Manually write a memory | `content`, `memory_type` |
| `memory_feedback` | Record agent feedback on a memory | `memory_id`, `rating` |
| `memory_stats` | Get tenant memory statistics | (none) |

---

## 1. `memory_distill`

The primary tool — runs the full 8-stage pipeline to extract, classify, score, filter, compress, embed, resolve, and persist memories from a conversation.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "conversation_id": {
      "type": "string",
      "description": "Unique identifier for the conversation session"
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
            "description": "Message content (plain text)"
          }
        },
        "required": ["role", "content"]
      },
      "description": "Array of conversation messages in chronological order"
    },
    "tenant_id": {
      "type": "string",
      "description": "Optional tenant identifier (overrides default)"
    }
  },
  "required": ["conversation_id", "messages"]
}
```

### Example Request

```json
{
  "conversation_id": "session-42",
  "messages": [
    {"role": "user", "content": "How can I improve query performance in SQLite?"},
    {"role": "assistant", "content": "Add indexes on columns used in WHERE clauses, use EXPLAIN QUERY PLAN to check, and consider WAL mode for concurrent reads."}
  ]
}
```

### Example Response

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

### Behavior

- Empty `messages[]` returns zero metrics, no error
- All messages must have non-empty `content`
- Messages that fail the noise/security filters are silently skipped
- Returns a JSON object with per-stage counts

---

## 2. `memory_compile`

Analyzes conversation messages to build a structured `SessionState`: goal, module, files, problems, decisions, and reasoning chain. Optionally runs distillation as a side effect.

### Input Schema

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
      "description": "Optional — include to also run distillation"
    },
    "tenant_id": {
      "type": "string"
    }
  },
  "required": ["messages"]
}
```

### Example Response

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"session\":{\"current_goal\":\"Improve SQLite query performance\",\"current_module\":\"store\",\"current_files\":[\"src/store.rs\"],\"open_problems\":[],\"reasoning_chain\":[{\"trigger\":\"User asked about indexing\",\"tool_name\":\"analyze\",\"tool_args\":\"EXPLAIN QUERY PLAN\",\"status\":\"success\",\"reasoning\":\"Added composite index on (user_id, created_at)\"}]},\"knowledge\":[],\"decisions\":[{\"decision\":\"Switch to WAL mode\",\"rationale\":\"Better concurrent read performance\",\"module\":\"store\",\"importance\":0.8}]}"
    }
  ],
  "is_error": false
}
```

### Detection Rules

| Component | Detection |
|---|---|
| `current_goal` | First user message |
| `current_module` | Module names from file paths in messages |
| `current_files` | Lines matching `src/...` patterns |
| `open_problems` | User questions with no matching assistant response |
| `knowledge` | Extracted problem-solution pairs |
| `decisions` | Lines containing `DONE`, `DECIDED`, `CHOSEN`, or `OPTED` |
| `reasoning_chain` | `tool_invocations` array on assistant messages |

---

## 3. `memory_search`

Searches stored memories using the configured retrieval mode (keyword, vector, or hybrid).

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Search query text"
    },
    "limit": {
      "type": "integer",
      "description": "Maximum results (default: configured retrieval_limit)"
    },
    "memory_type": {
      "type": "string",
      "description": "Optional filter: knowledge, skill, preference, experience, interaction, profile"
    },
    "tenant_id": {
      "type": "string"
    }
  },
  "required": ["query"]
}
```

### Example Response

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

### Scoring by Mode

| Mode | Score Composition |
|---|---|
| `keyword` | 0.7 × BM25 + 0.3 × importance |
| `vector` | cosine similarity |
| `hybrid` | 0.6 × semantic + 0.2 × keyword (BM25) + 0.2 × importance |

---

## 4. `memory_store`

Manually write a memory to the store without running the full distillation pipeline.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "content": {
      "type": "string",
      "description": "Memory content (expected in '问题：解决方案' format)"
    },
    "memory_type": {
      "type": "string",
      "description": "One of: knowledge, skill, preference, experience, interaction, profile"
    },
    "importance": {
      "type": "number",
      "description": "Importance score 0.0-1.0 (default: 0.5)"
    },
    "tenant_id": {
      "type": "string"
    }
  },
  "required": ["content", "memory_type"]
}
```

### Example Response

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

Record user/agent feedback on a previously stored memory. This can be used for reinforcement learning or manual curation.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "memory_id": {
      "type": "string",
      "description": "ID of the memory to provide feedback on"
    },
    "rating": {
      "type": "number",
      "description": "Rating 0.0-1.0 (0 = useless, 1 = very useful)"
    },
    "tenant_id": {
      "type": "string"
    }
  },
  "required": ["memory_id", "rating"]
}
```

### Example Response

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

Retrieve memory statistics for a tenant without exposing the actual memory contents.

### Input Schema

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

### Example Response

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

## Error Handling

All tools return errors in a consistent format:

```json
{
  "content": [
    {
      "type": "text",
      "text": "Error description"
    }
  ],
  "is_error": true
}
```

Common error categories:

| Category | Example |
|---|---|
| Invalid input | `"invalid input: missing field 'messages'"` |
| Not found | `"not found: memory 'abc123' does not exist"` |
| Storage | `"storage error: sqlite error: database is locked"` |
| Embedding | `"embedding service error: transport error: connection refused"` |
| Distillation | `"distillation error at phase 'extract': no messages"` |
