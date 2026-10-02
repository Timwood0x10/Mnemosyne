# Module: Retrieval Layer (src/retrieval.rs + store search)

> This document faithfully describes the retrieval implementation: what it
> does, how it works, and **why it is designed this way** (technical
> decisions). All diagrams are mermaid.

## 1. Overview

The retrieval layer answers one question: **given a query, find the most
relevant results from memory and the knowledge base.** Three modes
(`RetrievalMode`): `keyword` (FTS5 / BM25), `vector` (cosine similarity), and
`hybrid` (weighted merge). It powers `memory_search`, `inspect_entity` evidence
queries, and `persona_check` semantic matching.

```mermaid
flowchart TD
    Q["query"] --> RE["RetrievalEngine.search()"]

    RE --> KW["keyword mode"]
    RE --> VC["vector mode"]
    RE --> HY["hybrid mode"]

    KW --> F1["FTS5 MATCH (MEMORY_VECTOR_DIM=0)"]
    KW --> B1["BM25 full scan (no vectors)"]
    VC --> V1["cosine similarity<br/>sqlite-vec vec0 kNN"]
    HY --> M1["keyword + vector weighted merge"]

    F1 --> R["RetrievalResult list<br/>(score + content + meta)"]
    B1 --> R
    V1 --> R
    M1 --> R
```

## 2. Core structures

| Type | File | Responsibility |
|---|---|---|
| `RetrievalEngine` | `retrieval.rs` | orchestration: mode selection, external registry, merged scoring |
| `RetrievalResult` | `retrieval.rs` | one result: score + content + metadata |
| `bm25_score` | `retrieval.rs` | simplified BM25 (k1=1.2 only, tanh-normalized) |
| `search_by_vector` | `store/repository.rs` | kNN against the `vec_memories` `vec0` table (sqlite-vec) |
| `fts5_query` | `store.rs` | FTS5 MATCH query escaping |

## 3. Technical decisions (why)

### 3.1 Why keyword-first with optional vectors?

**Decision**: `MEMORY_VECTOR_DIM=0` (default) = pure keyword (FTS5); `>0`
enables vector search; `RetrievalMode` can select `vector` / `hybrid`.

**Why**:
- **Usable at zero embedding cost**: retrieval works with no embedding service —
  deploy, test, offline all viable.
- **Progressive enhancement**: add vectors (remote embedding provider such as
  OpenAI/Ollama) when semantic similarity is needed; the layer is transparent
  to both.

### 3.2 Why both FTS5 and BM25 for keywords?

**Decision** (`store.rs`): with `MEMORY_VECTOR_DIM=0` use FTS5 `MATCH`
(indexed); without vectors but needing full text, fall back to `bm25_score`
full scan (`search_by_keyword`).

**Why**:
- FTS5 is the indexed fast path (O(log n)); but FTS5 tokenization is weak for
  CJK (`unicode61` treats Chinese as whole runs), so BM25 full scan (token
  matching) is the Chinese fallback.
- Both normalize to [0,1] so merged scoring stays dimension-consistent.

### 3.3 Why does the store delegate vector search to sqlite-vec?

**Decision** (`store/repository.rs::search_by_vector`): kNN runs inside SQLite
against the `vec_memories` `vec0` virtual table (`vector MATCH ?1 AND k = ?2`),
rather than against an in-process index.

**Why**:
- **One source of truth**: a memory row and its vector are written by the same
  transaction, so a write can never leave the index and the rows disagreeing.
- **Tenant-correct top-k**: `k` is over-fetched to `limit × 16` *before* the
  tenant filter. kNN ranks globally, so a small tenant whose rows fall outside
  the global top-k would otherwise get zero results despite matching rows.

### 3.4 Why merge scores instead of intersecting results in hybrid mode?

**Decision** (`retrieval.rs`): keyword and vector scores merge with weights
(e.g. `0.6 × semantic + 0.2 × BM25 + 0.2 × importance`).

**Why**:
- Single signals have blind spots: keywords miss semantic paraphrase; vectors
  miss exact proper nouns. Merging improves recall and ranking.
- Entries without vectors (legacy rows) automatically fall back to keyword
  scoring — never silently dropped.

### 3.5 Why must FTS5 queries be escaped?

**Decision** (`store.rs::fts5_query`): escape FTS5-significant characters
(`"` `:` `(` etc.).

**Why**: FTS5 raises a syntax error on bare special characters — a user query
containing `:` (e.g. a filename) would fail entirely. Escaping makes arbitrary
input safe (security fix).

## 4. Deep dive

### 4.1 `RetrievalEngine` capabilities

- `new()`: assemble language/dimension configuration.
- `with_external_registry` / `set_external_registry`: mount the external
  knowledge registry so retrieval also covers sources attached via
  `knowledge_attach`.
- `search()`: dispatch by `mode` to keyword / vector / hybrid and return
  `RetrievalResult`s.

### 4.2 Scoring signals

| Signal | Source | Scale |
|---|---|---|
| keyword | `bm25_score` (simplified, k1=1.2 only, no length normalization) | tanh-normalized [0,1] |
| vector | sqlite-vec cosine distance, converted as `1 - distance` | [0,1] |
| importance | memory/fact importance | [0,1] |

### 4.3 Rows without an embedding

sqlite-vec reports cosine *distance* in `[0, 2]`; the engine converts it to a
similarity as `(1 - distance).clamp(0, 1)`. A memory with no stored embedding is
never inserted into `vec_memories`, so `search_by_vector` cannot return it — but
hybrid mode still reaches it through the keyword channel, so such rows are never
silently dropped. A stored all-zero vector is orthogonal and therefore scores
0.0, the same floor an absent embedding gets.

## 5. Related

- [System architecture](../en/architecture.md)
- [Knowledge storage](knowledge.md)
- [Cognition](cognition.md)
- [MCP framework](mcp.md)
