# Cognitive Memory MCP Server

An MCP server that extracts, stores, and retrieves memories from conversations using **sqlite-vec** for vector search.

## Features

- **Memory Distillation** — 8-stage pipeline: extract → classify → score → filter → embed → resolve conflicts → cap capacity → persist
- **Hybrid Retrieval** — BM25 keyword + vector cosine similarity + importance ranking
- **Multi-tenant** — tenant-isolated storage via MCP `tenant_id`
- **Pluggable Embeddings** — OpenAI, Ollama, or keyword-only mode (no embedding backend required)
- **MCP Tools** — `memory_distill`, `memory_search`, `memory_store`, `memory_feedback`, `memory_stats`

## Architecture

```
messages → Distiller → Classifier → Scorer → Filter → Embedder → Resolver → SQLiteVecStore
                                            ↓
                                     RetrievalEngine (keyword / vector / hybrid)
```

## Quick Start

```bash
# Build
make build

# Run in stdio mode (default)
make run

# Or directly
cargo run --bin memory-mcp -- serve

# Run tests
make test
```

## Configuration

All config via CLI args or env vars:

| Env var | Default | Description |
|---------|---------|-------------|
| `MEMORY_DB_PATH` | `./memory.db` | SQLite database path |
| `MEMORY_VECTOR_DIM` | `1024` | Embedding dimension |
| `MEMORY_EMBEDDING_PROVIDER` | `none` | `none`, `openai`, or `ollama` |
| `MEMORY_EMBEDDING_URL` | `http://localhost:8000` | Embedding service URL |
| `MEMORY_RETRIEVAL_MODE` | `keyword` | `keyword`, `vector`, or `hybrid` |
| `MEMORY_OPENAI_API_KEY` | — | Required when provider=openai |

## MCP Tools

| Tool | Description |
|------|-------------|
| `memory_distill` | Distill memories from a conversation |
| `memory_search` | Search memories (keyword/vector/hybrid) |
| `memory_store` | Manually store a memory |
| `memory_feedback` | Record agent feedback on a memory |
| `memory_stats` | Aggregate memory stats per tenant |

## Development

```bash
make check    # clippy + check
make fmt      # format code
make test     # run tests
make clean    # clean build artifacts
```
