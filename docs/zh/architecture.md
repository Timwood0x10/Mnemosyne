# Mnemosyne 系统架构

> 本文档**实事求是**地描述当前代码库（`src/`）的真实结构，所有图均为 mermaid 格式。
> 代码标识符、文件路径使用英文（与代码一致），叙述使用中文。

## 1. 总览

Mnemosyne 是一个**不依赖 LLM 的知识蒸馏引擎 + MCP 服务**：把叙事文本（小说/对话/散文）
编译为通用知识模型（文档 → 实体 → 事件 → 关系 → 证据），持久化到单一 SQLite 文件，
并通过 MCP 协议对外提供查询/蒸馏/人设保持工具。

```mermaid
flowchart TB
    subgraph CLI["CLI 入口 (src/main.rs)"]
        C1["serve — MCP 服务（stdio / http）"]
        C2["ingest — V1 语料蒸馏"]
        C3["migrate — V1 → 通用知识模型迁移"]
    end

    subgraph MCP["MCP 层 (src/mcp/)"]
        T["传输层: StdioTransport / HttpTransport"]
        S["MCPServer + ServerBuilder"]
        TOOLS["33 个工具（24 核心 + 9 个 V1 legacy）"]
    end

    subgraph COMP["编译流水线 (src/compiler/)"]
        P1["document.rs — 文档解析"]
        P2["chunk.rs — 分块"]
        P3["sentence.rs — 分句"]
        P4["profile.rs — Pass1 世界构建"]
        P5["extract.rs — Pass2 故事编译"]
        P6["resolver.rs — 别名/名称消解"]
        P7["timeline.rs / story_events.rs — 时间线与事件"]
        P8["entity/ — 实体注册表"]
    end

    subgraph KNOW["知识存储 (src/knowledge/)"]
        K1["store.rs — SQLiteKnowledgeStore"]
        K2["migration.rs — V1→通用迁移"]
        K3["memory_export.rs — 快照导入导出"]
    end

    subgraph COG["认知层 (src/cognition* / fact_store.rs)"]
        G1["cognition.rs — 事实类型/状态机"]
        G2["cognition_compiler.rs — 对话→事实"]
        G3["fact_store.rs — SqliteFactStore"]
    end

    subgraph RETR["检索 (src/retrieval.rs + src/vector/)"]
        R1["FTS5 关键词检索"]
        R2["HNSW / brute_force 余弦相似度"]
        R3["混合检索 hybrid"]
    end

    CLI --> MCP
    MCP --> COMP
    MCP --> COG
    COMP --> KNOW
    COG --> KNOW
    KNOW --> RETR
    RETR --> MCP
```

## 2. 模块结构（真实目录树）

```mermaid
graph TD
    A["src/ (lib: mnemosyne)"] --> B["main.rs — 入口 + 工具注册"]
    A --> C["compiler/ — 叙事编译流水线"]
    A --> D["mcp/ — MCP 框架与工具"]
    A --> E["knowledge/ — 知识存储/迁移/导出"]
    A --> F["ingest/ — V1 语料摄入"]
    A --> G["entity_resolver/ — 实体消解"]
    A --> H["persona/ — 人设检查/时间线"]
    A --> I["vector/ — HNSW + brute_force"]
    A --> J["storage/ — schema 定义"]
    A --> K["顶层: cognition / cognition_compiler / distiller / conversation_compiler / fact_store / retrieval / language / embed / ..."]

    C --> C1["document / sentence / chunk / profile / extract"]
    C --> C2["resolver / timeline / story_events / writer"]
    C --> C3["entity/ — EntityRegistry + JsonEntityProvider"]
    D --> D1["server.rs / transport.rs / types.rs"]
    D --> D2["memory_compile.rs / generalize_tool.rs / persona_check_tool.rs / ..."]
    E --> E1["store.rs (SQLiteKnowledgeStore)"]
    E --> E2["migration.rs (V1→通用)"]
```

## 3. MCP 服务架构

```mermaid
sequenceDiagram
    participant C as MCP 客户端 (IDE / Agent)
    participant T as Transport (stdio / http)
    participant S as MCPServer
    participant H as ToolHandler (29 个工具)
    participant K as SQLiteKnowledgeStore

    C->>T: JSON-RPC 2.0 消息
    T->>S: recv() 消息
    S->>S: 匹配方法 (initialize / tools/list / tools/call)
    S->>H: 分发 tools/call 到对应 handler
    H->>K: 查询/写入知识图谱
    K-->>H: 结果
    H-->>S: ToolCallResult (content + isError)
    S-->>T: send() JSONRPCResponse
    T-->>C: 响应
```

**传输层**（`Transport` trait）：

| 实现 | 用途 | 关键点 |
|---|---|---|
| `StdioTransport` | 本地 IDE 接入 | stdin/stdout 逐行 JSON-RPC |
| `HttpTransport` | 远程服务 | SSE 会话隔离（`x-mcp-session-id` 专属频道）、HTTP 强制 `--http-token` 鉴权、常量时间比较 |

## 4. 叙事编译流水线（compiler/）

```mermaid
flowchart LR
    A["corpus/*.txt"] --> B["document.rs 文档解析"]
    B --> C["chunk.rs 分块"]
    C --> D["sentence.rs 分句"]
    D --> E["profile.rs Pass1 世界构建<br/>(实体/别名/画像)"]
    E --> F["extract.rs Pass2 故事编译<br/>(事件/动作/证据)"]
    F --> G["resolver.rs + timeline.rs<br/>(别名消解/时间线)"]
    G --> H["writer.rs 写入知识库"]

    subgraph 消解器
        A1["resolver.rs 别名/名称消解"]
        A3["entity/ 实体注册表<br/>(JsonEntityProvider 字典)"]
    end

    E -.-> A1
    E -.-> A3
    F -.-> A1
```

**实体提供者**（`config/entity_profiles/*.json`）：`sanguo` / `shuihu` / `honglou` /
`xiyou` / `fengshen` / `warandpeace`，为每部小说提供规范实体名与别名。

## 5. 对话 → 认知（cognition 层）

```mermaid
flowchart TB
    A["messages[] (role/content)"] --> B["conversation_compiler.rs<br/>会话状态编译"]
    B --> C["cognition_compiler.rs<br/>观察 → 事实 (Fact)"]
    C --> D["fact_store.rs<br/>SqliteFactStore 持久化"]
    C --> E["distiller.rs<br/>长时记忆蒸馏"]
    E --> F["prompt.rs<br/>PromptBuilder 投影"]

    D --> G["检索 (retrieval)"]
    D --> H["persona/ 人设检查与时间线"]
```

**事实类型**（`cognition.rs`）：Identity / Preference / Goal / Event / Relationship / Emotion 等，
每条事实带证据链（EvidenceRef），不可变、可追踪。

## 6. 存储与检索

```mermaid
flowchart TD
    subgraph DB["SQLite 文件 (--db-path)"]
        T1["documents / chapters (通用知识模型)"]
        T2["knowledge_objects / knowledge_edges"]
        T3["evidence / mentions / compiler_runs"]
        T4["memories / vec_memories / memories_fts<br/>(SQLiteVecStore: vec0 + FTS5)"]
        T5["facts (SqliteFactStore)"]
    end

    Q["查询"] --> M1["keyword 模式 (FTS5 / BM25 全扫)"]
    Q --> M2["vector 模式 (余弦相似度)"]
    Q --> M3["hybrid 模式 (合并评分)"]
    M1 --> DB
    M2 --> DB
    M3 --> DB
```

| 组件 | 实现 | 说明 |
|---|---|---|
| `SQLiteKnowledgeStore` | `knowledge/store.rs` | 通用知识模型 CRUD（documents/objects/edges/evidence），事务包裹，FK 管理 |
| `SQLiteVecStore` | `store.rs` | 记忆存储：`vec0` 向量表 + `memories_fts` FTS5 表；`MEMORY_VECTOR_DIM=0` 时纯关键词 |
| `SqliteFactStore` | `fact_store.rs` | 认知事实持久化 |
| `RetrievalEngine` | `retrieval.rs` | 检索编排：`keyword`（FTS5/BM25）、`vector`（余弦）、`hybrid`（加权合并） |
| BM25 评分 | `retrieval.rs::bm25_score` | 简化 BM25 变体（仅 k1=1.2，无长度归一化），`tanh` 归一化到 [0,1] |
| 向量检索 | `vector/` HNSW / brute_force | 余弦相似度；零向量统一 `sqrt(2)`（cosine=0.0） |

## 7. 数据流全景

```mermaid
flowchart LR
    subgraph 输入
        I1["小说/散文 (txt/pdf)"]
        I2["对话 (json messages)"]
        I3["外部知识源 (attach)"]
    end

    subgraph 编译
        W1["compiler/ 叙事编译"]
        W2["cognition_compiler 对话编译"]
        W3["generalize_compile 任意源编译"]
    end

    subgraph 存储
        S1["知识图谱 (SQLite)"]
        S2["认知事实 (fact_store)"]
    end

    subgraph 对外
        O1["MCP 查询工具"]
        O2["MCP 蒸馏工具"]
        O3["人设保持工具"]
    end

    I1 --> W1 --> S1
    I2 --> W2 --> S2
    I3 --> W3 --> S1
    S1 --> O1
    S2 --> O2
    S1 --> O3
    S2 --> O3
```

## 8. 关键设计决策（实事求是）

1. **无 LLM 内核** — 编译、蒸馏、检索、人设检查全部为确定性算法（Aho-Corasick 动词匹配、
   bigram Jaccard 相似度、余弦向量检索、规则评分），零 API 依赖，行为可复现。
2. **单一 SQLite 文件** — 元数据、FTS5 索引、向量索引、认知事实全在一处，部署即一个文件。
3. **关键词优先、向量可选** — `MEMORY_VECTOR_DIM=0` 时零嵌入成本完全可用。
4. **双传输 MCP** — stdio 供本地 IDE，HTTP+SSE 供远程（会话隔离 + 强制鉴权 + 文件白名单）。
5. **两套存储分工** — `SQLiteKnowledgeStore`（叙事知识图谱）与 `SqliteFactStore`（对话认知事实），
   通过 `story_bridge` 等工具打通。
6. **V1 遗产保留** — `character_*` 表作为只读视图保留，`migrate` 将其迁入通用知识模型。

## 9. 相关文档

- [蒸馏流水线](distillation-pipeline.md)
- [存储与检索](storage-retrieval.md)
- [MCP 工具](mcp-tools.md)
- [配置](configuration.md)
- [快速开始](getting-started.md)
- [开发指南](development.md)
