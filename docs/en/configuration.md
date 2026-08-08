# Configuration

The server is configured via a combination of **environment variables**, **command-line flags**, and sensible **defaults**. The `Config` struct in `src/config.rs` loads from all sources with priorities:

```
CLI flags > Environment variables > Defaults
```

## Quick Reference

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `MEMORY_DB_PATH` | `./memory.db` | SQLite database file path |
| `MEMORY_VECTOR_DIM` | `0` | Vector dimension (0 = keyword-only FTS5) |
| `MEMORY_EMBEDDING_PROVIDER` | `none` | Embedding backend: `none`, `openai`, `ollama` |
| `MEMORY_EMBEDDING_URL` | `http://localhost:8000` | Embedding service endpoint (for remote providers) |
| `MEMORY_EMBEDDING_MODEL` | `e5-large` | Embedding model name |
| `MEMORY_EMBEDDING_TIMEOUT_MS` | `30000` | Embedding request timeout in milliseconds |
| `MEMORY_OPENAI_API_KEY` | — | OpenAI API key (required for `openai` provider) |
| `MEMORY_RETRIEVAL_MODE` | `keyword` | Search mode: `keyword`, `vector`, `hybrid` |
| `MEMORY_MIN_IMPORTANCE` | `0.6` | Minimum importance score to store a memory |
| `MEMORY_CONFLICT_THRESHOLD` | `0.85` | Cosine similarity threshold for conflict detection |
| `MEMORY_MAX_SOLUTIONS` | `5000` | Maximum `Knowledge` memories retained per tenant |
| `MEMORY_MAX_PER_DISTILL` | `3` | Maximum memories produced per distillation call |
| `MEMORY_DISABLE_CROSS_TURN` | `false` | Disable cross-turn 4-message arc extraction |
| `MEMORY_SSE_ADDR` | `""` | SSE listen address (empty = use stdio transport) |
| `RUST_LOG` | — | Logging level (e.g., `info`, `debug`, `memory_distill=debug`) |

### CLI Flags

| Flag | Environment Variable | Description |
|---|---|---|
| `--db-path <PATH>` | `MEMORY_DB_PATH` | Database file path |
| `--vector-dim <N>` | `MEMORY_VECTOR_DIM` | Vector dimension (0 = keyword) |
| `--embedding-provider <PROVIDER>` | `MEMORY_EMBEDDING_PROVIDER` | Embedding backend |
| `--embedding-url <URL>` | `MEMORY_EMBEDDING_URL` | Embedding endpoint |
| `--embedding-model <MODEL>` | `MEMORY_EMBEDDING_MODEL` | Model name |
| `--embedding-timeout-ms <MILLIS>` | `MEMORY_EMBEDDING_TIMEOUT_MS` | Request timeout (milliseconds) |
| `--retrieval-mode <MODE>` | `MEMORY_RETRIEVAL_MODE` | Search mode |
| `--min-importance <FLOAT>` | `MEMORY_MIN_IMPORTANCE` | Min importance for storage (default: 0.6) |
| `--conflict-threshold <FLOAT>` | `MEMORY_CONFLICT_THRESHOLD` | Conflict detection threshold (default: 0.85) |
| `--max-solutions <N>` | `MEMORY_MAX_SOLUTIONS` | Max Knowledge memories per tenant |
| `--max-per-distill <N>` | `MEMORY_MAX_PER_DISTILL` | Max memories per distillation call |
| `--disable-cross-turn` | `MEMORY_DISABLE_CROSS_TURN` | Disable cross-turn extraction |
| `--sse-addr <ADDR>` | `MEMORY_SSE_ADDR` | SSE listen address (empty = stdio) |

## Configuration Modes

### 1. Keyword-Only Mode (Zero API Cost)

```bash
cargo run --bin memory-mcp -- \
  --embedding-provider none \
  --retrieval-mode keyword \
  --db-path ./memory.db
```

| Setting | Value |
|---|---|
| Embeddings | Disabled (no API calls) |
| Search | FTS5 full-text keyword search |
| Vector dimension | 0 (not used) |
| API key | Not required |

Best for: local development, privacy-sensitive environments, cost-conscious deployments.

### 2. Hybrid Mode (Keyword + Vector)

```bash
MEMORY_OPENAI_API_KEY=sk-... cargo run --bin memory-mcp -- \
  --embedding-provider openai \
  --vector-dim 768 \
  --retrieval-mode hybrid
```

| Setting | Value |
|---|---|
| Embeddings | OpenAI API |
| Search | Hybrid (0.6 semantic + 0.2 keyword + 0.2 importance) |
| Vector dimension | 768 |
| API key | Required |

Best for: production deployments where retrieval quality matters.

### 3. Vector-Only Mode

```bash
MEMORY_OPENAI_API_KEY=sk-... cargo run --bin memory-mcp -- \
  --embedding-provider openai \
  --vector-dim 768 \
  --retrieval-mode vector
```

### 4. Ollama (Local Embeddings)

```bash
cargo run --bin memory-mcp -- \
  --embedding-provider ollama \
  --embedding-url http://localhost:11434 \
  --embedding-model nomic-embed-text \
  --vector-dim 768 \
  --retrieval-mode hybrid
```

## Validation Rules

The configuration is validated at startup. Common errors:

| Condition | Error |
|---|---|
| `vector_dim = 0` with `retrieval_mode = vector\|hybrid` | Rejected — set `vector_dim > 0` for vector/hybrid search |
| `provider = openai` without `MEMORY_OPENAI_API_KEY` | Rejected — API key required |
| `min_importance` outside `[0, 1]` | Rejected — must be between 0 and 1 |
| `conflict_threshold` outside `[0, 1]` | Rejected — must be between 0 and 1 |

## Logging

The server uses the `tracing` crate with `tracing-subscriber`. Control log verbosity with `RUST_LOG`:

```bash
RUST_LOG=info cargo run --bin memory-mcp
RUST_LOG=debug cargo run --bin memory-mcp
RUST_LOG=memory_distill=debug cargo run --bin memory-mcp
```

## Example: Production Configuration

```bash
export MEMORY_DB_PATH=/data/memory.db
export MEMORY_VECTOR_DIM=768
export MEMORY_EMBEDDING_PROVIDER=openai
export MEMORY_OPENAI_API_KEY=sk-...
export MEMORY_RETRIEVAL_MODE=hybrid
export MEMORY_MIN_IMPORTANCE=0.6
export MEMORY_MAX_SOLUTIONS=10000
export RUST_LOG=info

cargo run --bin memory-mcp
```
