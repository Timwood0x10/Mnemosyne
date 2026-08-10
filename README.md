# Mnemosyne — Memory Distillation Engine

> **Mnemosyne** — the Greek goddess of memory: memory never dies, is always
> reconstructable, and persists across sessions.

Compile any long-term interaction into an **evolving human cognitive model**: identity, preference, goal, emotion, relationship. Designed for companion AIs to maintain consistent persona across sessions.

> **Facts come from compilation, not guesswork.**
> **State comes from events, not prompts.**
> **Long-term cognition comes from the model, not context window length.**

---

## About the Name

**Mnemosyne** is the Greek goddess of memory — mother of the nine
Muses, keeper of everything that must not be forgotten. The name is a promise
made concrete by this engine:

- **Memory never dies** — facts are persisted in SQLite and survive
  across sessions, independent of the context window; `memory_export`/`memory_import`
  back them up and move them between machines intact.
- **Reconstructable** — every fact carries an evidence chain
  (EvidenceRef) and a scoring rule, so a persona's evolution timeline can be
  rebuilt from raw history at any time — nothing is guessed, everything is
  traceable.
- **Continuity across sessions** — cognition is *in the model,
  not in the prompt*: the store, not the context window, is the source of truth.

Just as Mnemosyne let poets and heroes *remember*, this engine lets a companion
AI *remember its user* — deterministically, without an LLM in the loop.

---

## Core Architecture

```
                Language Frontend
                     │
                     ▼
            Observation Compiler
                     │
                     ▼
            Knowledge Compiler
                     │
                     ▼
             Snapshot Builder
                     │
                     ▼
              Cognitive Context
```

### Compilation Pipeline

| Stage | Description |
|-------|-------------|
| **Language Frontend** | English/Chinese natural language parsing; extract Mentions, Actions, Evidence |
| **Observation Compiler** | Unified IR (Subject + Action + Object + Evidence), Aho-Corasick verb matching |
| **Knowledge Compiler** | Observation → Fact (immutable, persisted). FactType: Identity, Preference, Goal, Event, Relationship, Emotion, ... |
| **Snapshot Builder** | Facts → StateEngine → EntitySnapshot (Markdown/JSON) |
| **Cognitive Context** | Provide structured cognitive snapshot context to the AI agent |

### Core Principles

- **Facts from compilation**: All Facts are extracted from raw text by the compiler with evidence chain (EvidenceRef) tracing — no LLM guessing involved.
- **State from events**: An Entity's current state is aggregated from its Fact timeline, not from ad-hoc prompt construction.
- **Long-term cognition from the model**: Cognition state persists in SQLite, evolves across sessions, independent of context window size.

---

## MCP Tool Overview

### Cognitive State Tools

| Tool | Function | Required Params |
|------|----------|-----------------|
| `memory_compile` | Compile dialogue into structured Facts + cognitive state, optional distillation | `messages[]` |
| `cognitive_context` | Query entity cognitive snapshot: identity, preference, goal, events, relationships | `name` |
| `memory_context_check` | Proactive context-aware distillation: above threshold auto-compiles the conversation into facts + long-term memories + user profile; below threshold it is a read-only diagnostic | `messages[]` |

### Memory Distillation Tools

| Tool | Function | Required Params |
|------|----------|-----------------|
| `lore_scope` | 8-stage pipeline: extract → classify → score → filter → compress → embed → resolve → persist | `conversation_id`, `messages[]` |
| `memory_search` | Keyword / vector / hybrid retrieval | `query` |
| `memory_store` | Manually write memory | `content` |
| `memory_feedback` | Record Agent feedback on memories (for self-evolution) | `memory_id` |
| `memory_stats` | Tenant-level memory statistics | — |
| `memory_decay` | Deterministic memory decay/forgetting (down-weight + archive stale facts, never deletes) | — |

### Knowledge Ingestion / External Source Tools

| Tool | Function | Required Params |
|------|----------|-----------------|
| `generalize_compile` | Compile ANY external data (pasted conversation `doc_type=dialog` or raw prose `doc_type=text`) into the unified knowledge graph (documents/entities/edges/evidence) | `messages[]` or `text` |
| `agent_fact_compile` | Compile AI conversation into three-state facts (support/contradict/unrelated) | `messages[]` |
| `knowledge_attach` | Attach an external knowledge source (PDF/JSON/TXT/MD documents or JSON-backed DBs) as a searchable adapter | `source_name`, `path` |
| `knowledge_ingest` | Materialize an attached external source into the knowledge graph as documents + chapters + evidence | `source_name` |
| `memory_export` | Serialize the whole knowledge graph into a portable JSON snapshot (inline or to `path`) | — |
| `memory_import` | Replay a `memory_export` snapshot back into the store, deduplicating by identity | `content` or `path` |

### Mnemosyne Knowledge Query Tools

| Tool | Function | Required Params |
|------|----------|-----------------|
| `inspect_entity` | Query complete entity profile: attributes + relations + events + evidence | `name` |
| `timeline` | Event timeline (ordered by chapter) | `entity` |
| `relation_graph` | Relation graph BFS traversal (depth 1-5) | `entity` |
| `evidence` | Original text evidence search (keyword match) | `query` |
| `correct_relation` | Correct erroneous relations in knowledge graph | `source`, `predicate`, `old_target`, `new_target` |
| `person_key_events` | Distill a person's trajectory into key events (score + evidence) | `name` |
| `search_graph` | Structured graph search by name substring / object type / attribute value, scoped by document | `query` |
| `trace_path` | Shortest relationship path between two named entities (BFS over graph edges) | `source`, `target`, `max_depth` |

### Companion Persona Tools

Persona-consistency tools for companion AIs (keeping the persona stable across
sessions): inject a structured
persona card, guard a draft reply against the stored persona, track the agent↔user
relationship, and rebuild the persona's evolution timeline.

| Tool | Function | Required Params |
|------|----------|-----------------|
| `persona_inject` | Inject a structured, deterministic persona card (identity / persona / style / taboos / relationship) into the system prompt; multi-tenancy by `tenant_id` | `agent_id` |
| `persona_check` | Guard the agent's draft reply against the accumulated persona facts; report `conflicts` + `drift` (no LLM, keyword fallback) | `agent_id`, `draft` |
| `relationship_update` | Incrementally update the agent↔user relationship state from message emotion signals (intimacy / stage / trend / recent topics) | `agent_id`, `user_id`, `messages[]` |
| `relationship_query` | Read the current relationship snapshot for a tenant/agent/user triple | `agent_id`, `user_id` |
| `persona_timeline` | Rebuild an entity's persona evolution timeline (`origin → turning point → current state`) from accumulated facts (mem0 v3 ADD-only) | `entity_id` |
| `story_bridge` | Novel-character bridge: compile a protagonist's knowledge-graph story events into fact-store persona facts, providing cold-start baseline for `persona_timeline`/`persona_check` | `name` |
| `memory_decay` | Deterministic memory decay / forgetting: down-weight and archive stale facts, never delete (protects high-value persona facts) | — |

### V1 Legacy Character Tools

| Tool | Function | Note |
|------|----------|------|
| `character_search` | Search characters by name/attribute/novel | Read-only after migration; based on old `character_*` tables |
| `character_network` | Character relation graph BFS | Read-only after migration |
| `character_ingest` | Run four classics corpus distillation pipeline | Triggers full V1 extraction |
| `character_graph` | Export 3D character relation graph JSON | For visualization |

> `portrait_extract` (resume → person portrait) was removed in favor of the
> cognition-Facts conversation pipeline, which is the supported path for
> companion-AI persona profiling.

---

## Semantic Similarity Without an LLM

The system is deliberately LLM-free for retrieval and persona reasoning: all
semantic approximation is done with deterministic, offline algorithms, so the
server has zero API-key dependency and its behavior is reproducible across
runs. The techniques, in order of increasing semantic reach:

### 1. Keyword matching — FTS5

`MEMORY_VECTOR_DIM=0` (the default) enables SQLite FTS5 keyword search with
proper Unicode handling and escaped LIKE wildcards. Exact and substring
matches are answered directly from the index — O(log n), no embedding needed.

### 2. Vector retrieval — cosine similarity

When `MEMORY_VECTOR_DIM>0`, text is embedded once (local ONNX
`all-MiniLM-L6-v2`, 384-dim, via the `local-embed` feature, or a remote
provider) and queried by cosine similarity:

- `brute_force.rs` — exact O(N) full scan (the ground truth);
- `hnsw.rs` — approximate nearest neighbor for large graphs, with an agreed
  convention on degenerate inputs: a zero vector returns distance `sqrt(2)`
  (cosine = 0.0), identical to the brute-force scan, so both indexes agree.

### 3. Hybrid retrieval

`MEMORY_RETRIEVAL_MODE=hybrid` merges keyword hits and vector hits; results
without a vector (e.g. legacy rows) fall back to keyword-only scoring, so the
retrieval path never silently drops entries that lack an embedding.

### 4. Character-bigram Jaccard for names & aliases

For entity-name matching (`compiler/resolver.rs`) each name is embedded as its
**set of character bigrams** (e.g. `张三` → `{张三}`; `张飞` → `{张飞}`), and
similarity is the **Jaccard overlap** of the two bigram sets — no vector model
required. This catches transliteration/copy errors and partial-name aliases
deterministically.

### 5. Shared-bigram heuristics for persona consistency

`persona_check` (no-LLM path) compares a draft reply against stored persona
facts with **negation detection + shared-bigram overlap**: two statements
about the same bigram topic are flagged as conflict/drift without any model —
`我讨厌应酬` vs `我喜欢安稳` are correctly *not* marked as a stance flip
because they share fewer than 2 bigrams.

### 6. Rule-driven compilation

The whole extraction pipeline is rules, not inference: Aho-Corasick verb
matching (`observation_compiler`), deterministic event/relation builders
(`compiler/extract.rs`, `timeline.rs`), key-event importance scoring
(evidence richness + participant centrality + turning-point flag), and
rule-based relationship state updates (`relationship_update`) all run offline
and are fully reproducible.

> Net effect: every fact, edge, and retrieved result carries a traceable
> evidence chain or scoring rule — the "no LLM guessing" guarantee from
> [Core Principles](#core-principles) holds end to end.

---

## IDE Integration (MCP)

The server speaks standard MCP over stdio (or HTTP+SSE), so it plugs into any
IDE/agent that supports MCP clients — Claude Desktop, Cursor, VS Code,
JetBrains, or a custom agent framework.

### stdio (local, recommended for IDE)

```jsonc
// .mcp.json (VS Code / Cursor / Claude Desktop style)
{
  "mcpServers": {
    "mnemosyne": {
      "command": "cargo",
      "args": ["run", "--bin", "mnemosyne", "serve"],
      "cwd": "/abs/path/to/memory_distill"
    }
  }
}
```

> Tip: `cargo run` compiles on first connect. For a snappy IDE experience
> build once (`cargo build --release`) and point `command` at
> `target/release/mnemosyne` with `args: ["serve"]`.

### HTTP+SSE (remote, requires token)

The HTTP transport exposes two endpoints: `GET /sse` (Server-Sent Events
stream) and `POST /message` (JSON-RPC). Configure the MCP client with the SSE
endpoint URL:

```jsonc
{
  "mcpServers": {
    "mnemosyne": {
      "url": "http://host:5609/sse",
      "headers": {
        "Authorization": "Bearer <your-token>",
        "x-mcp-session-id": "<stable-id-per-client>"
      }
    }
  }
}
```

HTTP serving refuses to start without `--http-token` (see
[Configuration](#configuration)); each concurrent client should send a stable
`x-mcp-session-id` so its SSE stream only receives its own responses. For
remote deployments use `https://` — the bearer token would otherwise travel in
cleartext.

### What the AI can do once connected

- **Distill the current conversation**: the host passes the ongoing `messages[]`
  to `memory_compile` / `memory_context_check` and gets structured facts,
  decisions, session state and (optionally) long-term distilled memories —
  persisted to SQLite for cross-session continuity.
- **Query the knowledge graph**: `inspect_entity`, `timeline`, `relation_graph`,
  `search_graph`, `trace_path`, `evidence`.
- **Guard persona consistency**: `persona_check` / `persona_inject` /
  `relationship_update` keep a companion AI's persona from drifting.

> Note: MCP tools are called by the host — the server never auto-reads the
> conversation. To auto-distill every turn, instruct the host (system prompt or
> post-turn hook) to invoke `memory_compile` with the current messages.

---

## Installation

### Option 1 — Prebuilt binaries (recommended)

Download the binary for your platform from the
[Releases](https://github.com/Timwood0x10/Mnemosyne/releases) page:

| Platform | File |
|---|---|
| macOS (Apple Silicon) | `mnemosyne-aarch64-apple-darwin` |
| macOS (Intel) | `mnemosyne-x86_64-apple-darwin` |
| Linux (arm64) | `mnemosyne-aarch64-unknown-linux-gnu` |
| Linux (x86_64) | `mnemosyne-x86_64-unknown-linux-gnu` |
| Windows (x86_64) | `mnemosyne-x86_64-pc-windows-msvc.exe` |

```bash
# macOS / Linux
chmod +x mnemosyne-*
sudo mv mnemosyne-* /usr/local/bin/mnemosyne
mnemosyne --version

# Windows: rename to mnemosyne.exe and add its folder to PATH
```

### Option 2 — Build from source

Requires a Rust toolchain (see `rust-toolchain` / Cargo.toml for the MSRV).

```bash
git clone https://github.com/Timwood0x10/Mnemosyne.git
cd Mnemosyne
cargo build --release
./target/release/mnemosyne --version
```

Then continue with [Quick Start](#quick-start).

---

## Quick Start

```bash
# Zero config: SQLite only, no API key needed
cargo run --bin mnemosyne \
  --embedding-provider none \
  --retrieval-mode keyword \
  --db-path ./knowledge.db
```

### CLI Commands

| Command | Description |
|---------|-------------|
| `serve` (default) | Start MCP stdio server |
| `ingest --corpus-dir corpus` | Run V1 corpus distillation pipeline |
| `migrate --corpus-dir corpus` | Migrate V1 → General knowledge model |

### Testing

```bash
make check      # cargo clippy + cargo check (0 error, 0 warning)
make test       # 700+ unit + integration tests (nextest, ~18s)

# Full Romance of the Three Kingdoms compilation test
cargo test --test sanguo_compile e2e_sanguo -- --nocapture
```

### Test Corpora

All test corpora live in `corpus/` (text, PDF and dialog-JSON), with per-novel
entity profile packs in `config/entity_profiles/`.

| Corpus | Language | Kind | Used by |
|--------|----------|------|---------|
| `三国演义.txt` (Romance of the Three Kingdoms) | zh | novel text | `sanguo_compile`, `generalize_corpus_regression` |
| `水浒传.txt` (Water Margin) | zh | novel text | `generalize_corpus_regression` |
| `红楼梦.txt` (Dream of the Red Chamber) | zh | novel text | `honglou_compile`, `generalize_corpus_regression` |
| `西游记.txt` (Journey to the West) | zh | novel text | `xiyou` / `generalize_corpus_regression` |
| `封神演义.txt` (Investiture of the Gods) | zh | novel text | `fengshen_*`, `generalize_corpus_regression` |
| `大秦帝国.txt` (The Qin Empire) | zh | novel text | `daqin` |
| `倾城之恋.txt` (Love in a Fallen City) | zh | novella text | `qingcheng` / persona |
| `WarandPeace.txt` (War and Peace) | en | novel text (84k sentences) | `war_peace`, `war_mcp` (sampled: opening 100k chars) |
| `PrideAndPrejudice.txt` | en | novel text | `generalize_corpus_regression` |
| `巴黎圣母院.pdf` (Notre-Dame de Paris) | zh | PDF | e2e PDF (skipped if absent) |
| `2.pdf` | — | PDF | e2e PDF (skipped if absent) |
| `bailiusu_escape.json` (Bai Liusu flees the Bai household) | zh | dialog | `caoren_*`, companion MCP loop |
| `warpeace_pierre.json` (War and Peace · Pierre) | en | dialog | companion MCP loop |
| `raskolnikov_porfiry.json` | zh | dialog | `generalize_corpus_regression` (dialog path) |
| `sonia_raskolnikov.json` | zh | dialog | `generalize_corpus_regression` (dialog path) |
| `conversation_export_2026-08-02.json` | zh | dialog export | memory/migration tests |
| `ques.json` | zh | dialog | auxiliary |

Entity profile packs (`config/entity_profiles/`): `sanguo.json`, `shuihu.json`,
`honglou.json`, `xiyou.json`, `fengshen.json`, `warandpeace.json` — each maps
the novel's canonical entity names/aliases for the compiler's dictionary
(`JsonEntityProvider`).

> Slow full-corpus runs (e.g. the 7-novel `generalize_corpus_regression`) are
> `#[ignore]`d by default; run them explicitly with `--ignored`.


### Configuration

| Env Var | Default Value | Description |
|---------|---------------|-------------|
| `MEMORY_DB_PATH` | `./memory.db` | SQLite database path |
| `MEMORY_VECTOR_DIM` | `0` | 0 = pure keyword (FTS5), >0 = vector search |
| `MEMORY_EMBEDDING_PROVIDER` | `none` | `none` / `openai` / `ollama` |
| `MEMORY_RETRIEVAL_MODE` | `keyword` | `keyword` / `vector` / `hybrid` |
| `FACTION_MAP_PATH` | `config/faction_map.json` | Faction map configuration |

### Local ONNX Embedding (`--features local-embed`)

The project ships a self-contained ONNX embedder — **no remote server, no API
key, nothing to deploy**:

- Provider: `FastEmbedProvider` (`src/entity_resolver/embedding.rs`)
- Model: `all-MiniLM-L6-v2` (ONNX local, **384-dim**), downloaded once on
  first use and cached locally (~90 MB); offline afterwards.
- Enable: build/test with the `local-embed` Cargo feature:

```bash
cargo test --features local-embed --test real_embed_probe   # real-embed probe
cargo build --features local-embed                          # enable at build
```

- The `RemoteEmbedder` path (`MEMORY_EMBEDDING_PROVIDER=openai|ollama`) is the
  **alternative** that needs an upstream server; the self-contained ONNX path
  is the zero-deployment default for local use.

### Development

```bash
make check      # cargo clippy + cargo check
make test       # All 320+ tests
make fmt        # Format code
```

### Module Documentation

| Module | English | Chinese |
|--------|---------|------|
| System architecture | [docs/en/architecture.md](docs/en/architecture.md) | [docs/zh/architecture.md](docs/zh/architecture.md) |
| Narrative compilation pipeline | [docs/en/compiler.md](docs/en/compiler.md) | [docs/zh/compiler.md](docs/zh/compiler.md) |
| MCP framework & tools | [docs/en/mcp.md](docs/en/mcp.md) | [docs/zh/mcp.md](docs/zh/mcp.md) |
| Knowledge storage layer | [docs/en/knowledge.md](docs/en/knowledge.md) | [docs/zh/knowledge.md](docs/zh/knowledge.md) |
| Cognition layer | [docs/en/cognition.md](docs/en/cognition.md) | [docs/zh/cognition.md](docs/zh/cognition.md) |
| Retrieval layer | [docs/en/retrieval.md](docs/en/retrieval.md) | [docs/zh/retrieval.md](docs/zh/retrieval.md) |

---

## License

Apache-2.0
