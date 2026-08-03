# HCC — Human Cognition Compiler

Compile any long-term interaction into an **evolving human cognitive model**: identity, preference, goal, emotion, relationship. Designed for companion AIs to maintain consistent persona across sessions.

> **Facts come from compilation, not guesswork.**
> **State comes from events, not prompts.**
> **Long-term cognition comes from the model, not context window length.**

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

### Memory Distillation Tools

| Tool | Function | Required Params |
|------|----------|-----------------|
| `lore_scope` | 8-stage pipeline: extract → classify → score → filter → compress → embed → resolve → persist | `conversation_id`, `messages[]` |
| `memory_search` | Keyword / vector / hybrid retrieval | `query` |
| `memory_store` | Manually write memory | `content` |
| `memory_feedback` | Record Agent feedback on memories (for self-evolution) | `memory_id` |
| `memory_stats` | Tenant-level memory statistics | — |

### LoreScope Knowledge Query Tools

| Tool | Function | Required Params |
|------|----------|-----------------|
| `inspect_entity` | Query complete entity profile: attributes + relations + events + evidence | `name` |
| `timeline` | Event timeline (ordered by chapter) | `entity` |
| `relation_graph` | Relation graph BFS traversal (depth 1-5) | `entity` |
| `evidence` | Original text evidence search (keyword match) | `query` |
| `correct_relation` | Correct erroneous relations in knowledge graph | `source`, `predicate`, `old_target`, `new_target` |
| `person_key_events` | Distill a person's trajectory into key events (score + evidence) | `name` |

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

## Quick Start

```bash
# Zero config: SQLite only, no API key needed
cargo run --bin lore-scope \
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
make test       # 320+ unit + integration tests

# Full Romance of the Three Kingdoms compilation test
cargo test --test sanguo_compile e2e_sanguo -- --nocapture
```

### Project Structure

```
src/
├── main.rs                      # Entry point + 15 MCP tool registration
├── conversation_compiler.rs     # Agent dialogue compiler (Fact extraction)
├── cognition.rs                 # Core types: Observation, Fact, FactType, StateEngine, Snapshot
├── fact_store.rs                # SQLite FactStore (Fact persistence)
│
├── observation_compiler.rs      # Observation Compiler: Aho-Corasick + DefaultRule
├── language.rs                  # Language Frontend (English/Chinese LanguageProvider)
│
├── compiler/                    # LoreScope Compiler (corpus processing pipeline)
│   ├── mod.rs                  # CompileContext + IR types
│   ├── document.rs             # Document Parser
│   ├── sentence.rs             # Sentence Compiler
│   ├── chunk.rs                # Chunk Planner
│   ├── profile.rs              # Pass 1: World Builder
│   ├── extract.rs              # Pass 2: Story Compiler
│   ├── alias.rs                # Alias Resolver
│   ├── pronoun.rs              # Pronoun Resolver
│   ├── relation.rs             # Relation Builder
│   ├── timeline.rs             # Timeline Builder
│   ├── inference.rs            # Rule Engine
│   ├── merge.rs                # Chunk Merge
│   ├── writer.rs               # Store Writer
│   └── entity/                 # Entity Registry
│
├── knowledge/                  # Knowledge storage layer
│   ├── store.rs                # SQLiteKnowledgeStore
│   └── migration.rs            # V1 → General model migration
│
├── mcp/                        # MCP framework
│   ├── server.rs               # JSON-RPC 2.0 server
│   ├── transport.rs            # Stdio transport layer
│   ├── types.rs                # JSON-RPC types
│   └── knowledge_tools.rs      # Knowledge query MCP tools
│
├── distiller.rs                # Memory Distillation pipeline
├── store.rs                    # SQLiteVecStore
├── retrieval.rs                # Retrieval engine
├── embed.rs                    # Embedding service
├── config.rs                   # Configuration
└── config/entity_profiles/     # Entity configuration profiles
```

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

---

## License

Apache-2.0
