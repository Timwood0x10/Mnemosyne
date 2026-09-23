# Mnemosyne — Cognitive Memory Engine for Persistent AI Companions

> **Mnemosyne** — the Greek goddess of memory: memory never dies, is always
> reconstructable, and persists across sessions.

Compile any long-term interaction into an **evolving human cognitive model**: identity, preference, goal, emotion, relationship. Designed for companion AIs to maintain consistent persona across sessions.

> Memory remembers what happened. Cognition remembers what it means.

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
| `state_timeline` | Reconstruct how an entity's state emerged: per-dimension state intervals (validity window + evidence) plus deterministic transitions (`gradual_change`/`stance_flip`/`behavioral_confirmation`) | `entity_id`, `dimension?`, `tenant_id?` |
| `fact_provenance` | Audit why a fact is believed: confidence, epistemic status (active/superseded/contradicted), original-text evidence, and the `derived_from` derivation chain | `fact_id`, `tenant_id?` |

### Decision Tools

First-class decisions: what a speaker committed to, why, and what happened
afterwards. Decisions are produced by compilation — `memory_compile` turns
explicit commitments ("我答应…" / "I promise…") into `Decision` rows, anchored
to an Event fact for the utterance — and read back through these two tools.

| Tool | Function | Required Params |
|------|----------|-----------------|
| `decision_trace` | Trace a decision back to the facts that supported it (supporting evidence, not causality) | `decision_id`, `tenant_id?` |
| `decision_search` | Keyword search a subject's decisions over verb/object, newest first | `subject`, `keyword?`, `limit?`, `tenant_id?` |

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

When `MEMORY_VECTOR_DIM>0` and an embedding provider is configured
(`MEMORY_EMBEDDING_PROVIDER=openai|ollama`, a remote API), text is embedded
and queried by cosine similarity:

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

### Option 1 — Install script (recommended)

`scripts/install.sh` detects your platform/architecture, downloads the
matching release archive, and unpacks it into a `.mnemosyne/` directory:

```bash
# macOS / Linux (bash)
curl -fsSL https://raw.githubusercontent.com/Timwood0x10/Mnemosyne/main/scripts/install.sh | bash

# Or clone and run it locally
git clone https://github.com/Timwood0x10/Mnemosyne.git
cd Mnemosyne
./scripts/install.sh          # latest release
./scripts/install.sh v0.1.2   # a specific version
```

What it produces — everything lives in one directory, so the binary always
finds its resources next to itself:

```
~/.mnemosyne/
├── mnemosyne            # the binary (mnemosyne.exe on Windows)
├── markers_zh.json      # Chinese observation-marker word list (editable)
└── markers_en.json      # English observation-marker word list (editable)
```

Run it:

```bash
~/.mnemosyne/mnemosyne serve
```

### Option 2 — Manual download

Download the archive for your platform from the
[Releases](https://github.com/Timwood0x10/Mnemosyne/releases) page:

| Platform | Archive |
|---|---|
| macOS (Apple Silicon) | `mnemosyne-aarch64-apple-darwin.tar.gz` |
| macOS (Intel) | `mnemosyne-x86_64-apple-darwin.tar.gz` |
| Linux (arm64) | `mnemosyne-aarch64-unknown-linux-gnu.tar.gz` |
| Linux (x86_64) | `mnemosyne-x86_64-unknown-linux-gnu.tar.gz` |
| Windows (x86_64) | `mnemosyne-x86_64-pc-windows-msvc.tar.gz` |

```bash
# macOS / Linux
mkdir -p ~/.mnemosyne && tar -xzf mnemosyne-<platform>.tar.gz -C ~/.mnemosyne
chmod +x ~/.mnemosyne/mnemosyne
~/.mnemosyne/mnemosyne --version

# Windows: extract with your archive tool, then run mnemosyne.exe
```

### Customizing the marker word lists

`markers_zh.json` and `markers_en.json` decide which words in a conversation
produce facts. Each file maps an action (`feel`, `plan`, `want`, `dislike`,
`belief`, `stuck`, `life_event`, …) to a list of trigger words. Edit them to:

- add your own vocabulary (modern slang, domain terms, personal quirks);
- remove words that cause false positives;
- tune what the engine extracts about the user.

No recompile needed — just edit the JSON and restart. If the files are absent
the binary falls back to built-in defaults.

### Option 3 — Build from source

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
make check      # cargo clippy + cargo check (0 errors)
make test       # 780+ unit + integration tests (nextest, ~1s warm)
```

The suite is **self-contained**: each test builds its own synthetic corpus and
in-memory SQLite store, so it passes on a clean checkout with no fixtures and no
network. The cognition-layer end-to-end test drives the **real MCP JSON-RPC
path** (`tools/call` over an in-memory transport) rather than calling handlers
directly:

```bash
cargo test --test cognitive_state_e2e -- --nocapture
```

### Local corpora (optional)

`corpus/` holds large third-party texts used for **manual / ad-hoc**
verification (`ingest --corpus-dir corpus`, `migrate --corpus-dir corpus`). It is
gitignored, and **the default test suite does not depend on it** — the
corpus-driven regression tests were removed so CI never fails on a missing
fixture.

| File | Language | Kind |
|------|----------|------|
| `三国演义.txt` (Romance of the Three Kingdoms) | zh | novel text |
| `水浒传.txt` (Water Margin) | zh | novel text |
| `红楼梦.txt` (Dream of the Red Chamber) | zh | novel text |
| `西游记.txt` (Journey to the West) | zh | novel text |
| `封神演义.txt` (Investiture of the Gods) | zh | novel text |
| `大秦帝国.txt` (The Qin Empire) | zh | novel text |
| `倾城之恋.txt` (Love in a Fallen City) | zh | novella text |
| `WarandPeace.txt` (War and Peace) | en | novel text |
| `PrideAndPrejudice.txt` (Pride and Prejudice) | en | novel text |
| `巴黎圣母院.pdf` / `1.pdf` / `2.pdf` | zh / — | PDF |
| `bailiusu_escape.json` (Bai Liusu flees the Bai household) | zh | dialog |
| `warpeace_pierre.json` (War and Peace · Pierre) | en | dialog |
| `raskolnikov_porfiry.json`, `sonia_raskolnikov.json` | zh | dialog |
| `conversation_export_2026-08-02.json` | zh | dialog export |
| `ques.json` | zh | auxiliary |

Entity profile packs (`config/entity_profiles/`): `sanguo.json`, `shuihu.json`,
`honglou.json`, `xiyou.json`, `fengshen.json`, `warandpeace.json` — each maps
the novel's canonical entity names/aliases for the compiler's dictionary
(`JsonEntityProvider`).


### Configuration

| Env Var | Default Value | Description |
|---------|---------------|-------------|
| `MEMORY_DB_PATH` | `./memory.db` | SQLite database path |
| `MEMORY_VECTOR_DIM` | `0` | 0 = pure keyword (FTS5), >0 = vector search |
| `MEMORY_EMBEDDING_PROVIDER` | `none` | `none` / `openai` / `ollama` |
| `MEMORY_RETRIEVAL_MODE` | `keyword` | `keyword` / `vector` / `hybrid` |
| `FACTION_MAP_PATH` | `config/faction_map.json` | Faction map configuration |

### Development

```bash
make check      # cargo clippy + cargo check
make test       # All 780+ tests
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
