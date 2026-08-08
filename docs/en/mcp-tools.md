# MCP Tools Reference

The server exposes **10 MCP tools** via the Model Context Protocol. Each tool is a registered handler with an input schema validated at the JSON-RPC layer. The first 6 tools (`memory_*`) handle conversation distillation; the last 4 (`character_*`) manage the classical-novel character knowledge graph.

## Tool Overview

| Tool | Description | Required Arguments |
|---|---|---|
| `memory_distill` | Run the 8-stage distillation pipeline | `conversation_id`, `messages[]` |
| `memory_compile` | Build session state (optionally distill) | `messages[]` |
| `memory_search` | Search stored memories | `query` |
| `memory_store` | Manually write a memory | `content`, `memory_type` |
| `memory_feedback` | Record agent feedback on a memory | `memory_id` |
| `memory_stats` | Get tenant memory statistics | (none) |
| `character_search` | Search characters by name/attribute/novel | `query` |
| `character_network` | BFS-traverse the character relationship graph | `name` |
| `character_ingest` | Distill character graph from novel corpus | (none) |
| `character_graph` | Export 3D character graph JSON for visualization | (none) |

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
            "enum": ["user", "assistant", "system"]
          },
          "content": {
            "type": "string",
            "description": "Message content (plain text)"
          },
          "tool_call_id": {
            "type": "string",
            "description": "Tool call identifier for tool result messages"
          },
          "turn_id": {
            "type": "string",
            "description": "Turn identifier for grouping messages"
          }
        },
        "required": ["role", "content"]
      },
      "description": "Array of conversation messages in chronological order"
    },
    "tenant_id": {
      "type": "string",
      "description": "Optional tenant identifier (overrides default)"
    },
    "user_id": {
      "type": "string",
      "description": "Optional user identifier"
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
          "role": {"type": "string", "enum": ["user", "assistant", "system"]},
          "content": {"type": "string"}
        },
        "required": ["role", "content"]
      }
    },
    "distill": {
      "type": "boolean",
      "default": false,
      "description": "Also run the distillation pipeline"
    },
    "conversation_id": {
      "type": "string",
      "description": "Required when distill=true"
    },
    "tenant_id": {
      "type": "string",
      "default": "default"
    },
    "user_id": {
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
      "default": 5,
      "description": "Maximum results (default: 5)"
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
      "enum": ["knowledge", "preference", "skill", "experience", "interaction", "profile"],
      "default": "knowledge",
      "description": "One of: knowledge, skill, preference, experience, interaction, profile"
    },
    "confidence": {
      "type": "number",
      "default": 0.5,
      "description": "Confidence score 0.0-1.0 (default: 0.5)"
    },
    "tenant_id": {
      "type": "string",
      "default": "default"
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
    "useful": {
      "type": "boolean",
      "default": true,
      "description": "Whether the memory was useful (true = useful, false = not useful)"
    },
    "tenant_id": {
      "type": "string",
      "default": "default"
    }
  },
  "required": ["memory_id"]
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

## 7. `character_search`

Searches the character knowledge graph by name, alias, clothing, personality, or description. Optionally includes events and relations for each matched character.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "query": {"type": "string", "description": "Search query – matches name, clothing, personality, description"},
    "tenant_id": {"type": "string", "default": "novels"},
    "novel": {"type": "string", "description": "Optional novel filter (e.g. 水浒传, 西游记, 三国演义, 红楼梦)"},
    "limit": {"type": "integer", "default": 10},
    "include_events": {"type": "boolean", "default": false, "description": "Include events for each character"},
    "include_relations": {"type": "boolean", "default": false, "description": "Include relations for each character"}
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
      "text": "{\"results\":[{\"id\":\"...\",\"name\":\"扈三娘\",\"novel\":\"水浒传\",\"aliases\":[\"一丈青\"],\"clothing\":\"...\",\"personality\":\"...\",\"description\":\"登场水浒传共12回\",\"importance\":0.4}],\"stats\":{\"total_characters\":92,\"total_events\":340,\"total_relations\":128}}"
    }
  ],
  "is_error": false
}
```

---

## 8. `character_network`

Traverses the character relationship graph via BFS, starting from a named character and expanding outward up to `depth` hops. Returns a recursive tree of characters, their events, their relations, and their connections.

Each `CharacterRelation` in the response carries the full dimensional scoring `metadata` bag (`co_occurrence_score`, `event_coupling_score`, `relation_type_score`, etc.) populated by `character_ingest`, so callers can rank or filter edges without re-running the pipeline. See [§10 `character_graph`](#10-character_graph) for the dimensional breakdown.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "name": {"type": "string", "description": "Starting character name"},
    "tenant_id": {"type": "string", "default": "novels"},
    "novel": {"type": "string", "description": "Optional novel filter"},
    "depth": {"type": "integer", "default": 2, "description": "Traversal depth (1-5, default 2)"}
  },
  "required": ["name"]
}
```

### Example Response

Returns a `CharacterNetworkNode` tree:

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"character\":{\"name\":\"扈三娘\",...},\"events\":[...],\"relations\":[...],\"connections\":[{\"character\":{\"name\":\"林冲\",...},\"events\":[...],\"relations\":[...],\"connections\":[]}]}"
    }
  ],
  "is_error": false
}
```

---

## 9. `character_ingest`

Runs the full character distillation pipeline over the four classical Chinese novels (水浒传, 三国演义, 红楼梦, 西游记). Reads `.txt` files from the corpus directory, splits them into chapters, extracts character appearances/events/descriptions, detects relationships via keyword proximity, and persists everything into the character store.

**This is a heavy operation** — expect 30-60 seconds on the full corpus (~8MB of text). Run it once to populate the store, then use `character_search` / `character_network` / `character_graph` to query.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "corpus_dir": {"type": "string", "default": "corpus", "description": "Path to directory containing novel .txt files"}
  }
}
```

### Example Response

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

### Extraction Pipeline

| Stage | Description |
|---|---|
| Chapter split | Parse `第N回` markers, Chinese numeral → integer |
| Alias matching | Longest-first overlap resolution across all character aliases |
| Event extraction | Sentences with strong verbs (杀/斩/擒/战...) near character name |
| Description | Clothing keywords (头戴/身穿/铠甲...) + personality keywords (相貌/性格/勇猛...) |
| Death detection | Keywords (死/亡/卒/阵亡...) within 100 chars of name |
| Co-occurrence | Characters appearing in the same chapter → relation weight |
| Relation typing | Keyword proximity (夫妻/结义/父子/师徒/君臣/仇敌/挚友/姐妹/亲戚), fallback "关联". The pipeline re-evaluates the relation type across every chapter where a pair co-occurs and locks in the first specific (non-"关联") type it detects, so an oath in chapter 1 is not lost to a generic meeting scene in an earlier chapter. |
| Dimensional scoring | Each relation stores three independent scores in `metadata`: `co_occurrence_score` (frequency / 15), `event_coupling_score` (shared events / total events), and `relation_type_score` (1.0 for typed, 0.3 for "关联"). The combined `importance` is `0.5·co + 0.3·event + 0.2·type`, clamped to `[0, 1]`. |
| Importance | Characters: `min(1.0, chapters/30)`. Relations: combined dimensional score (see above). |

---

## 10. `character_graph`

Exports the full 3D character relationship graph as structured JSON for frontend visualization. Returns **nodes** (characters with multi-dimensional attributes) and **edges** (relations with weights).

This is the "立体人物关系网络": each node carries appearance (`clothing`), psychology (`personality`), and composite description dimensions; each edge carries a relation type and a dimensional weight score. The graph supports the pattern **character → events → related characters** with dimensional scoring between relations.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "tenant_id": {"type": "string", "default": "novels"},
    "novel": {"type": "string", "description": "Optional novel filter (e.g. 水浒传)"},
    "max_nodes": {"type": "integer", "default": 200, "description": "Maximum nodes to return"}
  }
}
```

### Example Response

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

### Graph Structure

```
Node dimensions:
  ├── appearance   (clothing / 外貌)
  ├── personality  (心理 / 性格)
  └── description  (composite summary)

Edge dimensions:
  ├── relation_type        (夫妻/结义/父子/师徒/君臣/仇敌/挚友/姐妹/亲戚/关联)
  ├── weight               (combined importance = 0.5·co + 0.3·event + 0.2·type)
  ├── chapter              (temporal anchor where the relation was detected)
  └── dimensions           (per-edge dimensional breakdown)
      ├── co_occurrence_score  (count / 15, frequency-based strength)
      ├── event_coupling_score (shared events / total events, semantic coupling)
      ├── relation_type_score  (1.0 for typed relations, 0.3 for generic "关联")
      ├── co_occurrence_count   (raw co-occurrence chapter count)
      ├── shared_event_count    (events where both characters are mentioned)
      └── detected_at_chapter   (chapter where the type keyword was matched)
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
