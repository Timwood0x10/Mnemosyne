# Mnemosyne System Architecture

> This document **faithfully** describes the current codebase (`src/`) as it
> actually is. All diagrams are mermaid. Identifiers and paths follow the code;
> prose is in English.

## 1. Overview

Mnemosyne is an **LLM-free knowledge distillation engine + MCP server**: it
compiles narrative text (novels / dialogs / prose) into a unified knowledge
model (documents → entities → events → relations → evidence), persists it to a
single SQLite file, and exposes query / distillation / persona-guard tools over
the MCP protocol.

```mermaid
flowchart TB
    subgraph CLI["CLI entry (src/main.rs)"]
        C1["serve — MCP server (stdio / http)"]
        C2["ingest — V1 corpus distillation"]
        C3["migrate — V1 → unified knowledge model"]
    end

    subgraph MCP["MCP layer (src/mcp/)"]
        T["Transport: StdioTransport / HttpTransport"]
        S["MCPServer + ServerBuilder"]
        TOOLS["33 tools (24 core + 9 V1 legacy)"]
    end

    subgraph COMP["Compilation pipeline (src/compiler/)"]
        P1["document.rs — document parsing"]
        P2["chunk.rs — chunk planning"]
        P3["sentence.rs — sentence splitting"]
        P4["profile.rs — Pass 1 world building"]
        P5["extract.rs — Pass 2 story compilation"]
        P6["resolver.rs — alias/name resolution"]
        P7["timeline.rs / story_events.rs — timeline & events"]
        P8["entity/ — entity registry"]
    end

    subgraph KNOW["Knowledge storage (src/knowledge/)"]
        K1["store.rs — SQLiteKnowledgeStore"]
        K2["migration.rs — V1 → unified migration"]
        K3["memory_export.rs — snapshot export/import"]
    end

    subgraph COG["Cognition layer (src/cognition* / fact_store.rs)"]
        G1["cognition.rs — fact types / state engine"]
        G2["cognition_compiler.rs — conversation → facts"]
        G3["fact_store.rs — SqliteFactStore"]
    end

    subgraph RETR["Retrieval (src/retrieval.rs + src/vector/)"]
        R1["FTS5 keyword search"]
        R2["HNSW / brute_force cosine similarity"]
        R3["hybrid retrieval"]
    end

    CLI --> MCP
    MCP --> COMP
    MCP --> COG
    COMP --> KNOW
    COG --> KNOW
    KNOW --> RETR
    RETR --> MCP
```

## 2. Module structure (actual directory tree)

```mermaid
graph TD
    A["src/ (lib: mnemosyne)"] --> B["main.rs — entry + tool registration"]
    A --> C["compiler/ — narrative compilation pipeline"]
    A --> D["mcp/ — MCP framework & tools"]
    A --> E["knowledge/ — knowledge storage / migration / export"]
    A --> F["ingest/ — V1 corpus ingestion"]
    A --> G["entity_resolver/ — entity resolution"]
    A --> H["persona/ — persona check / timeline"]
    A --> I["vector/ — HNSW + brute_force"]
    A --> J["storage/ — schema definitions"]
    A --> K["top-level: cognition / cognition_compiler / distiller / conversation_compiler / fact_store / retrieval / language / embed / ..."]

    C --> C1["document / sentence / chunk / profile / extract"]
    C --> C2["resolver / timeline / story_events / writer"]
    C --> C3["entity/ — EntityRegistry + JsonEntityProvider"]
    D --> D1["server.rs / transport.rs / types.rs"]
    D --> D2["memory_compile.rs / generalize_tool.rs / persona_check_tool.rs / ..."]
    E --> E1["store.rs (SQLiteKnowledgeStore)"]
    E --> E2["migration.rs (V1 → unified)"]
```

## 3. MCP server architecture

```mermaid
sequenceDiagram
    participant C as MCP client (IDE / Agent)
    participant T as Transport (stdio / http)
    participant S as MCPServer
    participant H as ToolHandler (29 tools)
    participant K as SQLiteKnowledgeStore

    C->>T: JSON-RPC 2.0 message
    T->>S: recv() message
    S->>S: dispatch method (initialize / tools/list / tools/call)
    S->>H: route tools/call to handler
    H->>K: query / write knowledge graph
    K-->>H: result
    H-->>S: ToolCallResult (content + isError)
    S-->>T: send() JSONRPCResponse
    T-->>C: response
```

**Transport layer** (`Transport` trait):

| Implementation | Use case | Key points |
|---|---|---|
| `StdioTransport` | local IDE integration | line-delimited JSON-RPC over stdin/stdout |
| `HttpTransport` | remote serving | per-session SSE isolation (`x-mcp-session-id`), mandatory `--http-token` auth, constant-time comparison |

## 4. Narrative compilation pipeline (compiler/)

```mermaid
flowchart LR
    A["corpus/*.txt"] --> B["document.rs parse"]
    B --> C["chunk.rs plan"]
    C --> D["sentence.rs split"]
    D --> E["profile.rs Pass 1 world building<br/>(entities / aliases / profiles)"]
    E --> F["extract.rs Pass 2 story compilation<br/>(events / actions / evidence)"]
    F --> G["resolver.rs + timeline.rs<br/>(alias resolution / timeline)"]
    G --> H["writer.rs persist to store"]

    subgraph resolvers
        A1["resolver.rs alias/name resolution"]
        A3["entity/ registry<br/>(JsonEntityProvider dictionaries)"]
    end

    E -.-> A1
    E -.-> A3
    F -.-> A1
```

**Entity providers** (`config/entity_profiles/*.json`): `sanguo` / `shuihu` /
`honglou` / `xiyou` / `fengshen` / `warandpeace` — canonical names and aliases
per novel.

## 5. Conversation → cognition (cognition layer)

```mermaid
flowchart TB
    A["messages[] (role/content)"] --> B["conversation_compiler.rs<br/>session-state compilation"]
    B --> C["cognition_compiler.rs<br/>observations → facts"]
    C --> D["fact_store.rs<br/>SqliteFactStore persistence"]
    C --> E["distiller.rs<br/>long-term memory distillation"]
    E --> F["prompt.rs<br/>PromptBuilder projection"]

    D --> G["retrieval"]
    D --> H["persona/ check & timeline"]
```

**Fact types** (`cognition.rs`): Identity / Preference / Goal / Event /
Relationship / Emotion — each fact carries an evidence chain (EvidenceRef),
immutable and traceable.

## 6. Storage & retrieval

```mermaid
flowchart TD
    subgraph DB["SQLite file (--db-path)"]
        T1["documents / chapters (unified knowledge model)"]
        T2["knowledge_objects / knowledge_edges"]
        T3["evidence / mentions / compiler_runs"]
        T4["memories / vec_memories / memories_fts<br/>(SQLiteVecStore: vec0 + FTS5)"]
        T5["facts (SqliteFactStore)"]
    end

    Q["query"] --> M1["keyword mode (FTS5 / BM25 full scan)"]
    Q --> M2["vector mode (cosine)"]
    Q --> M3["hybrid mode (merged scoring)"]
    M1 --> DB
    M2 --> DB
    M3 --> DB
```

| Component | Implementation | Notes |
|---|---|---|
| `SQLiteKnowledgeStore` | `knowledge/store.rs` | unified knowledge model CRUD (documents/objects/edges/evidence), transactional, FK-managed |
| `SQLiteVecStore` | `store.rs` | memory storage: `vec0` vector table + `memories_fts` FTS5 table; pure keyword when `MEMORY_VECTOR_DIM=0` |
| `SqliteFactStore` | `fact_store.rs` | cognitive fact persistence |
| `RetrievalEngine` | `retrieval.rs` | retrieval orchestration: `keyword` (FTS5/BM25), `vector` (cosine), `hybrid` (weighted merge) |
| BM25 scoring | `retrieval.rs::bm25_score` | simplified BM25 variant (k1=1.2 only, no length normalization), `tanh`-normalized to [0,1] |
| Vector search | `vector/` HNSW / brute_force | cosine similarity; zero vector → `sqrt(2)` (cosine=0.0) |

## 7. End-to-end data flow

```mermaid
flowchart LR
    subgraph inputs
        I1["novels / prose (txt/pdf)"]
        I2["dialogs (json messages)"]
        I3["external knowledge sources (attach)"]
    end

    subgraph compile
        W1["compiler/ narrative compilation"]
        W2["cognition_compiler dialog compilation"]
        W3["generalize_compile arbitrary-source compilation"]
    end

    subgraph store
        S1["knowledge graph (SQLite)"]
        S2["cognitive facts (fact_store)"]
    end

    subgraph expose
        O1["MCP query tools"]
        O2["MCP distillation tools"]
        O3["persona-guard tools"]
    end

    I1 --> W1 --> S1
    I2 --> W2 --> S2
    I3 --> W3 --> S1
    S1 --> O1
    S2 --> O2
    S1 --> O3
    S2 --> O3
```

## 8. Key design decisions (as implemented)

1. **LLM-free core** — compilation, distillation, retrieval and persona checks
   are all deterministic algorithms (Aho-Corasick verb matching, bigram-Jaccard
   similarity, cosine vector search, rule scoring). Zero API-key dependency,
   reproducible behavior.
2. **Single SQLite file** — metadata, FTS5 index, vector index and cognitive
   facts live in one place; deployment is one file.
3. **Keyword-first, vector optional** — fully usable at zero embedding cost
   with `MEMORY_VECTOR_DIM=0`.
4. **Dual-transport MCP** — stdio for local IDEs, HTTP+SSE for remote
   (session isolation + mandatory auth + file allowlist).
5. **Two stores with clear roles** — `SQLiteKnowledgeStore` (narrative
   knowledge graph) and `SqliteFactStore` (dialog cognitive facts), bridged by
   tools such as `story_bridge`.
6. **V1 legacy preserved** — `character_*` tables remain a read-only view;
   `migrate` moves them into the unified knowledge model.

## 9. Related documents

- [Distillation pipeline](distillation-pipeline.md)
- [Storage & retrieval](storage-retrieval.md)
- [MCP tools](mcp-tools.md)
- [Configuration](configuration.md)
- [Getting started](getting-started.md)
- [Development](development.md)
