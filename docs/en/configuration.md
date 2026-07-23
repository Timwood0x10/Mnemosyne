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
| `MEMORY_EMBEDDING_BASE_URL` | `http://localhost:11434` | Embedding service endpoint (for remote providers) |
| `MEMORY_EMBEDDING_MODEL` | `text-embedding-ada-002` | Embedding model name |
| `MEMORY_EMBEDDING_TIMEOUT` | `30` | Embedding request timeout in seconds |
| `MEMORY_OPENAI_API_KEY` | — | OpenAI API key (required for `openai` provider) |
| `MEMORY_RETRIEVAL_MODE` | `keyword` | Search mode: `keyword`, `vector`, `hybrid` |
| `MEMORY_RETRIEVAL_LIMIT` | `10` | Default max results per search |
| `MEMORY_MIN_IMPORTANCE` | `0.10` | Minimum importance score to store a memory |
| `MEMORY_CONFLICT_THRESHOLD` | `0.92` | Cosine similarity threshold for conflict detection |
| `MEMORY_MAX_MEMORIES_PER_TYPE` | `5000` | Per-type capacity limit per tenant |
| `MEMORY_TENANT_ID` | `default` | Default tenant ID |
| `MEMORY_CROSS_TURN_EXTRACTION` | `true` | Enable 4-message arc extraction |
| `MEMORY_TOP_N_PREFILTER` | `200` | Pre-filter to top-N before embedding |
| `MEMORY_FINAL_TOP_N` | `100` | Final cap on memories persisted per call |
| `RUST_LOG` | — | Logging level (e.g., `info`, `debug`, `memory_distill=debug`) |

### CLI Flags

| Flag | Environment Variable | Description |
|---|---|---|
| `--db-path <PATH>` | `MEMORY_DB_PATH` | Database file path |
| `--vector-dim <N>` | `MEMORY_VECTOR_DIM` | Vector dimension (0 = keyword) |
| `--embedding-provider <PROVIDER>` | `MEMORY_EMBEDDING_PROVIDER` | Embedding backend |
| `--embedding-base-url <URL>` | `MEMORY_EMBEDDING_BASE_URL` | Embedding endpoint |
| `--embedding-model <MODEL>` | `MEMORY_EMBEDDING_MODEL` | Model name |
| `--embedding-timeout <SECS>` | `MEMORY_EMBEDDING_TIMEOUT` | Request timeout (seconds) |
| `--retrieval-mode <MODE>` | `MEMORY_RETRIEVAL_MODE` | Search mode |
| `--retrieval-limit <N>` | `MEMORY_RETRIEVAL_LIMIT` | Max search results |
| `--min-importance <FLOAT>` | `MEMORY_MIN_IMPORTANCE` | Min importance for storage |
| `--conflict-threshold <FLOAT>` | `MEMORY_CONFLICT_THRESHOLD` | Conflict detection threshold |
| `--max-memories-per-type <N>` | `MEMORY_MAX_MEMORIES_PER_TYPE` | Per-type capacity |
| `--tenant-id <ID>` | `MEMORY_TENANT_ID` | Tenant identifier |

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
  --embedding-base-url http://localhost:11434 \
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
export MEMORY_RETRIEVAL_LIMIT=20
export MEMORY_MIN_IMPORTANCE=0.15
export MEMORY_MAX_MEMORIES_PER_TYPE=10000
export MEMORY_TENANT_ID=prod-tenant-1
export RUST_LOG=info

cargo run --bin memory-mcp
```
