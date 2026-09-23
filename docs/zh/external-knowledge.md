# 外部知识接入

Mnemosyne 除了从内置语料（四大名著）蒸馏人物图谱外，还支持接入**四类外部知识**，统一走编译管线 + 图谱 + 多信号检索，不绕过编译器裸注入原始文本（dev_guide「事实来自编译」）。

本指南面向希望挂载外部文档、外部 DB 或已有 AI 对话的用户，介绍三种新增 MCP 工具与两种增强工具的用法。

---

## 1. 支持的外部知识形态

| 形态 | 处理策略 | 是否物化 | 证据义务 | 接入方式 |
|---|---|---|---|---|
| **文档/语料** | 编译器：分句→实体→事件→图谱 | 物化 | 原文偏移 | `knowledge_attach` + `knowledge_ingest` |
| **词表/词典** | `ExternalFileProvider`（只辅助，无 effects） | 不落库 | 来源+许可证 | 既有机制（本指南不展开） |
| **外部 DB（索引型）** | 查询转发外部 DB，结果作检索信号 | 不物化 | 表+行 | `knowledge_attach`（`source_type=db`） |
| **AI 对话（user/agent/derived）** | 三态归属编译为 Fact | 物化 | 消息+偏移 / tool_call 响应 | `agent_fact_compile` |

**决策规则**：真相源是 Mnemosyne → 物化；真相源是外部系统（DB）→ 索引型（不双写）。

---

## 2. 支持的文档格式

`knowledge_attach`（`source_type=document`）通过扩展名自动识别格式：

| 扩展名 | 格式 | 输出 `doc_type` | 说明 |
|---|---|---|---|
| `.txt` | Text | `text` | 每文件一个文档 |
| `.md` / `.markdown` | Markdown | `markdown` | 每文件一个文档 |
| `.json` | JSON | `json` | 一个文件可含多个文档 |
| `.pdf` | PDF | `pdf` | 纯 Rust 抽取文本（FlateDecode + `BT…ET`） |

未知扩展名回退为纯文本，确保缺失扩展名不会阻断接入。

### JSON 文档格式

JSON loader 接受三种形状：

**裸数组：**

```json
[
  {"title": "第一章", "text": "正文内容...", "chapter": 1, "author": "佚名"},
  {"title": "第二章", "text": "正文内容...", "chapter": 2}
]
```

**`documents` 包装：**

```json
{
  "documents": [
    {"title": "第一章", "text": "正文内容..."}
  ]
}
```

**单对象：**

```json
{"title": "唯一文档", "text": "正文内容..."}
```

每个条目的字段：

| 字段 | 必需 | 说明 |
|---|---|---|
| `text` | 是 | 文档正文 |
| `title` | 否 | 标题，缺省时用文件名 |
| `chapter` | 否 | 章节号（整数） |
| `source` | 否 | 来源标识，缺省时用文件路径 |
| `author` | 否 | 作者 |

### PDF 抽取说明

PDF 文本抽取为**尽力而为**（best-effort）实现，聚焦于简单的文本型 PDF：

- 通过 `flate2` 解压 `FlateDecode` 流并解析 `BT…ET` 块内的文本算子。
- **不支持**：自定义字体编码、加密 PDF、扫描件（图片型 PDF）、复杂排版还原。
- 抽取失败时返回明确错误，不会静默吞掉。

---

## 3. 新增 MCP 工具

### 3.1 `knowledge_attach`

将外部知识源注册为可检索适配器。文档（PDF/JSON/TXT/MD）为**物化型**（需配合 `knowledge_ingest` 写入图谱）；JSON-DB 为**索引型**（查询时转发融合到 hybrid 检索）。挂载后自动重建实体链接器，使 `inspect_entity` 能解析跨来源名称。

#### 输入模式

```json
{
  "type": "object",
  "properties": {
    "source_type": {
      "type": "string",
      "enum": ["document", "db"],
      "description": "document = PDF/JSON/TXT/MD 文件；db = JSON-DB 文件"
    },
    "path": {"type": "string", "description": "文件路径（source_type=document 时必填）"},
    "connection": {"type": "string", "description": "JSON-DB 文件路径（source_type=db 时必填）。文件须为 [{id, text, score?}] 数组或 {\"documents\": [...]}"},
    "source_name": {"type": "string", "description": "可选友好名称；缺省时用文件名"},
    "entity_links": {
      "type": "array",
      "description": "可选的跨来源实体链接 [{external_name, canonical_name, source}]",
      "items": {
        "type": "object",
        "properties": {
          "external_name": {"type": "string"},
          "canonical_name": {"type": "string"},
          "source": {"type": "string"}
        },
        "required": ["external_name", "canonical_name", "source"]
      }
    }
  },
  "required": ["source_type"]
}
```

#### 示例：挂载 PDF 文档

```json
{
  "source_type": "document",
  "path": "/data/research/lorescope-paper.pdf",
  "source_name": "lorescope-paper",
  "entity_links": [
    {"external_name": "Mnemosyne", "canonical_name": "Mnemosyne 系统", "source": "lorescope-paper"}
  ]
}
```

响应：

```json
{
  "source_name": "lorescope-paper",
  "source_type": "document",
  "format": "pdf",
  "documents_loaded": 1,
  "entity_links_registered": 1,
  "mode": "materialize-only (documents are NOT query-forwarded; use knowledge_ingest to materialize into the graph)"
}
```

#### 示例：挂载 JSON-DB（索引型）

```json
{
  "source_type": "db",
  "connection": "/data/factions.json",
  "source_name": "faction-db"
}
```

`/data/factions.json` 的内容：

```json
[
  {"id": "wei", "text": "曹魏，三国时期北方政权", "score": 0.9},
  {"id": "shu", "text": "蜀汉，刘备建立的政权", "score": 0.85}
]
```

响应：

```json
{
  "source_name": "faction-db",
  "source_type": "db",
  "connection": "/data/factions.json",
  "rows_indexed": 2,
  "entity_links_registered": 0,
  "mode": "index-mode (query-forwarded into hybrid search via RRF; not materialized by default)"
}
```

#### 行为

- 文档读取与 PDF 解压经 `tokio::task::spawn_blocking` 移出 tokio worker 线程，不阻塞异步运行时。
- `source_name` 缺省时从文件名推导，保证 provenance 可读。
- 索引型 DB 的查询为大小写不敏感子串匹配，按 `(score desc, id asc)` 排序，保证确定性。
- 每次挂载后立即重建 `EntityLinker`，`inspect_entity` 随即能看到新的跨来源名称。

---

### 3.2 `knowledge_ingest`

将已挂载的物化型源写入知识图谱：每个 `ExternalDoc` 变为 `Document` + `Chapter` + `Evidence` 行，使既有的 `evidence` / `inspect_entity` 工具能查询到这些内容。

#### 输入模式

```json
{
  "type": "object",
  "properties": {
    "source_name": {"type": "string", "description": "已挂载的源名称，或 'all' 表示所有源"},
    "mode": {
      "type": "string",
      "enum": ["index", "materialize"],
      "default": "materialize",
      "description": "index = 仅报告状态；materialize = 持久化到图谱"
    }
  },
  "required": ["source_name"]
}
```

#### 示例：物化单个源

```json
{"source_name": "lorescope-paper", "mode": "materialize"}
```

响应：

```json
{
  "source_name": "lorescope-paper",
  "mode": "materialize",
  "documents_created": 1,
  "chapters_created": 1,
  "evidence_created": 1,
  "total_materialized": 1
}
```

#### 示例：物化所有已挂载源

```json
{"source_name": "all"}
```

#### 行为

- **幂等**：同名文档不重复写入（`find_document_by_title` 命中即复用既有 `doc_id`），符合 dev_guide「不双写」精神。
- `index` 模式仅报告当前信号提供方数量，不执行物化（适用于索引型源的状态查询）。
- 章节号取自 `ExternalDoc.chapter`，缺省为 1。

---

### 3.3 `agent_fact_compile`

将一段 AI 对话编译为**三态事实**（user / agent / derived）并持久化。User 事实始终持久化；agent + derived 通道需要显式开启（`include_agent_facts=true`），保证 agent 永不替用户表态（plan §C2）。

#### 三态归属

| 通道 | 来源 | 归属实体 | attribution | confidence |
|---|---|---|---|---|
| `user_facts` | user 消息中的偏好/目标/情绪 | User | （无标记） | 原值 |
| `agent_facts` | assistant 的 tool 调用或完成语 | Agent（`entity_type=agent`） | `agent` | 原值 |
| `derived_facts` | agent 转述用户认知（"you like"/"你喜欢"/"the user wants"…） | User | `agent_derived` | 打折 ×0.5 |

**零污染不变式**：User 的 Preference / Goal / Emotion 只来自 user 消息；`user_facts` 永不含 `attribution` 标记。agent 与 user 实体通过命名空间隔离的 `external_key`（`agent:<id>`）区分，永不碰撞。

#### 输入模式

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
          "content": {"type": "string"},
          "tool_call_id": {"type": "string"},
          "turn_id": {"type": "string"}
        },
        "required": ["role", "content"]
      }
    },
    "tenant_id": {"type": "string", "default": "default"},
    "user_id": {"type": "string"},
    "agent_id": {"type": "string"},
    "include_agent_facts": {
      "type": "boolean",
      "default": false,
      "description": "开启 agent + derived 事实通道（opt-in）"
    }
  },
  "required": ["messages"]
}
```

#### 示例：仅编译 user 事实（默认）

```json
{
  "messages": [
    {"role": "user", "content": "我喜欢用 Rust 写后端，性能很重要"},
    {"role": "assistant", "content": "好的，我记下了。"}
  ],
  "user_id": "alice",
  "agent_id": "mnemosyne"
}
```

响应：

```json
{
  "tenant_id": "default",
  "user_id": "alice",
  "agent_id": "mnemosyne",
  "user_entity_id": 12,
  "agent_entity_id": 13,
  "include_agent_facts": false,
  "user_facts_persisted": 1,
  "agent_facts_persisted": 0,
  "derived_facts_persisted": 0,
  "total_extracted": 1,
  "note": "agent channel disabled (default); only user facts persisted. set include_agent_facts=true to enable agent + derived channels"
}
```

#### 示例：开启 agent 通道

```json
{
  "messages": [
    {"role": "user", "content": "帮我查一下三国演义里曹操的官渡之战"},
    {"role": "assistant", "content": "已执行查询", "tool_call_id": "tool-1", "turn_id": "t1"},
    {"role": "user", "content": "我喜欢战略分析"},
    {"role": "assistant", "content": "你喜欢的战略分析能力，我会记住。"}
  ],
  "user_id": "alice",
  "agent_id": "mnemosyne",
  "include_agent_facts": true
}
```

此时：
- user 通道提取「喜欢战略分析」→ User 偏好事实。
- agent 通道把 tool 调用（`tool_call_id=tool-1`）编译为 Event 事实，归属 Agent 实体，`attribution=agent`，证据指向 tool 响应片段。
- derived 通道把 agent 的转述「你喜欢的战略分析能力」编译为 User 偏好事实，`attribution=agent_derived`，confidence 打折 0.5，且需**双重证据**（agent 转述 + 前置 user 消息）。

---

## 4. 增强的 MCP 工具

### 4.1 `memory_search`（hybrid 检索融合外部信号）

`hybrid` 模式下，检索引擎把外部 DB（索引型）的命中作为**第三路 RRF 候选**与本地关键词、语义结果融合：

- 外部命中以 `ext:<source>:<id>` 命名空间标识，携带 `is_external` / `external_score` 字段。
- 通过 Reciprocal Rank Fusion 融合，高分外部命中可超过弱本地命中，同时保持 scale-free 排序。
- 无 registry 挂载时行为完全不变；`keyword` 模式不受影响。

### 4.2 `inspect_entity`（跨来源实体画像）

查询前先经 `EntityLinker` 解析外部名到统一实体：

- 已链接的外部名（如 "John Smith" / "J. Smith"）解析到 canonical（如 "Mr. Smith"）。
- 未知 surface 回退原值，纯本地实体不受影响。
- 响应新增 `external_aliases: [{source, external_name}]`，承载跨来源画像（向后兼容：空数组时 `skip_serializing_if` 不输出）。

`EntityLinker` 的解析策略：

| 索引 | 查找方式 | 冲突策略 |
|---|---|---|
| `source_map` | `(source, external_name)` 精确匹配 | last-wins（支持源重新挂载纠正旧映射） |
| `surface_map` | 仅按 `external_name` 回退 | first-wins（保证 canonical 稳定） |

---

## 5. 典型工作流

### 工作流 A：接入一份 PDF 研究报告

```
1. knowledge_attach   (source_type=document, path=report.pdf)
2. knowledge_ingest   (source_name=report, mode=materialize)
3. memory_search      (query="...", 模式=hybrid)   ← 命中已物化的图谱内容
4. inspect_entity     (entity=...)                 ← 经 EntityLinker 解析
```

### 工作流 B：接入外部 DB 作为检索信号（不物化）

```
1. knowledge_attach   (source_type=db, connection=factions.json)
2. memory_search      (query="曹魏", 模式=hybrid)   ← 外部命中经 RRF 融合，不写图谱
```

### 工作流 C：蒸馏已有 AI 对话

```
1. agent_fact_compile (messages=[...], include_agent_facts=false)  ← 仅 user 事实
   — 或 —
1. agent_fact_compile (messages=[...], include_agent_facts=true)   ← user + agent + derived
2. inspect_entity     (entity=user:alice)    ← 查看 user 认知，无 agent 污染
```

---

## 6. 设计原则与非目标

### 设计原则

- **事实来自编译**：外部文档必须经编译器（分句→实体→事件→图谱），不绕过编译器裸注入原始文本。
- **不双写**：外部 DB 默认索引型（查询转发），不物化、不同步，避免真相源漂移。
- **agent 不替用户表态**：User 的 Preference / Goal / Emotion 只来自 user 消息；agent 转述必须带 `attribution=agent_derived` + 双重证据 + confidence 打折。
- **实体隔离**：agent 实体用 `external_key=agent:<id>` 命名空间，与 user 实体永不碰撞。
- **可追溯**：每个图谱节点可追溯来源（文档偏移 / DB 表行 / tool_call_id）。

### 明确非目标

- 不做外部 DB 的自动物化同步（保持索引型默认，避免双写）。
- 不让 agent 替用户表态（agent 不产生 User 的 Preference / Goal / Emotion）。
- 不绕过编译器裸注入原始文本。
- 不内置商业词典（仅授权适配器）。

---

## 7. 实现参考

本文档即该能力的实施与验收记录（早期版本曾指向 `plan/external-knowledge-plan.md`，该文件
已不在仓库中）。对外接口见 [MCP 工具清单](./mcp-tools.md)。核心模块：

| 模块 | 职责 |
|---|---|
| `src/knowledge/adapter.rs` | `KnowledgeAdapter` / `ExternalSignalProvider` trait，`DocumentAdapter` / `DbAdapter` / `VectorAdapter` |
| `src/knowledge/format.rs` | Text / Markdown / JSON / PDF 四格式 loader |
| `src/knowledge/pdf.rs` | 纯 Rust PDF 文本抽取（`flate2`） |
| `src/knowledge/external.rs` | `ExternalKnowledgeRegistry`（运行时挂载、RRF 融合、linker 重建） |
| `src/knowledge/entity_linker.rs` | 跨来源实体链接与解析 |
| `src/agent_facts.rs` | `ConversationFacts` 三态提取 |
| `src/mcp/external_knowledge_tools.rs` | 三个新增 MCP 工具的处理器 |
