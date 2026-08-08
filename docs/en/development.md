# Development Guide

## Project Structure

```
memory_distill/
├── Cargo.toml              # Package manifest, dependencies, features
├── Makefile                # Common development commands
├── src/
│   ├── main.rs             # Server wiring, 6 tool handlers, build_server()
│   ├── lib.rs              # Crate root, module re-exports
│   ├── classifier.rs       # MemoryClassifier — deterministic keyword scoring
│   ├── compiler.rs         # ConversationCompiler — session state builder
│   ├── config.rs           # Config — CLI args, env vars, validation
│   ├── detector.rs         # QuestionDetector — is_problem heuristic
│   ├── distiller.rs        # PipelineDistiller — 8-stage orchestrator
│   ├── embed.rs            # EmbeddingService trait + NullEmbedder + RemoteEmbedder
│   ├── error.rs            # Error — unified error types
│   ├── extractor.rs        # ExperienceExtractor — problem-solution pairs
│   ├── filter.rs            # NoiseFilter + SecurityFilter
│   ├── prompt.rs           # PromptBuilder — structured context injection
│   ├── resolver.rs         # ConflictResolver — cosine similarity + importance comparison
│   ├── retrieval.rs        # RetrievalEngine — BM25, hybrid scoring
│   ├── scorer.rs           # ImportanceScorer — [0,1] importance scoring
│   ├── store.rs            # SQLiteVecStore — sqlite-vec + FTS5
│   ├── types.rs            # Domain types: Memory, Experience, SessionState, etc.
│   └── mcp/
│       ├── mod.rs          # MCP module re-exports
│       ├── server.rs       # MCPServer — JSON-RPC dispatcher
│       ├── transport.rs    # StdioTransport — stdin/stdout I/O
│       └── types.rs        # JSON-RPC 2.0 types
├── docs/
│   ├── en/                 # English documentation
│   └── zh/                 # Chinese documentation
└── examples/
    └── claude-desktop-config.json
```

## Development Commands

```bash
make check      # Runs: cargo clippy --all-targets --all-features + cargo check
make test       # Runs: cargo test (142 unit tests + 3 doctests)
make run        # Runs: cargo run --bin memory-mcp -- ...
make build      # Runs: cargo build --release
```

### Manual Commands

```bash
# Run all tests with output
cargo test -- --nocapture

# Run a specific test
cargo test distill_simple_pair -- --nocapture

# Run clippy
cargo clippy --all-targets --all-features -- -D warnings

# Build release
cargo build --release
```

## Test Coverage

The project has **142 unit tests + 3 doctests**. Tests are organized per module:

| Module | What's Tested |
|---|---|
| `distiller.rs` | Full pipeline, compression, Chinese text, capacity control |
| `store.rs` | CRUD, FTS5 search, vector search, tenant isolation, batch ops |
| `retrieval.rs` | BM25 scoring, tokenization, hybrid ranking, tenant filtering |
| `classifier.rs` | All MemoryType classifications, case-insensitivity, fallback |
| `scorer.rs` | Length curve, keyword cap, type bias ordering, clamping |
| `filter.rs` | Noise rejection, security pattern detection, custom patterns |
| `resolver.rs` | Cosine similarity, conflict replacement, dimension mismatch |
| `extractor.rs` | Direct/cross-turn extraction, empty input, multiple pairs |
| `compiler.rs` | Goal detection, file tracking, decisions, reasoning chain |
| `types.rs` | Memory TTL, display text preference, round-trip serde |
| `config.rs` | CLI parsing, validation rules, env override |
| `mcp/types.rs` | ContentBlock, ToolCallResult, JSON-RPC message round-trips |
| `error.rs` | Display formatting, error type conversions |
| `embed.rs` | RemoteEmbedder construction |
| `prompt.rs` | Knowledge/decision display, recent message injection |

## Adding a New Feature

### Adding an MCP Tool

1. Define your tool struct in `src/main.rs`:
   ```rust
   struct MyNewTool {
       // dependencies
   }
   ```

2. Implement `ToolHandler`:
   ```rust
   #[async_trait]
   impl ToolHandler for MyNewTool {
       async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
           // parse args, do work, return result
       }
   }
   ```

3. Register in `build_server()`:
   ```rust
   .register_tool("my_tool", "Description", input_schema, Arc::new(MyNewTool { ... }))
   ```

### Adding a Pipeline Stage

1. Add the phase function to `PipelineDistiller` in `src/distiller.rs`
2. Insert it into the `distill()` method in the correct position
3. Add relevant counters to `DistillationMetrics`
4. Write tests for the new stage behavior

## Code Conventions

- All code is documented with English comments
- Public functions have doc comments with argument/return/error documentation
- Tests follow the pattern: `/// Objective: ...` + `/// Invariants: ...`
- Constants use `SCREAMING_SNAKE_CASE`
- No unwrap/expect outside tests and main()
- Error handling uses `thiserror` in library code, `anyhow` in binary code

## Feature Flags

| Feature | Default | Description |
|---|---|---|
| `remote-embed` | Yes | Enables HTTP-based embedding providers (`reqwest` dependency). Disable with `--no-default-features` for fully offline builds. |

## Debugging

### Logging

Set `RUST_LOG` for verbose output:

```bash
RUST_LOG=debug cargo run --bin memory-mcp -- --db-path /tmp/debug.db
RUST_LOG=memory_distill=debug,rusqlite=info cargo run --bin memory-mcp
```

### In-Memory Database

For testing without file I/O:

```rust
let store = SQLiteVecStore::open_in_memory(dim).await?;
```

### Common Issues

| Issue | Likely Cause | Fix |
|---|---|---|
| `sqlite-vec` init fails | Missing bundled extension | Ensure `sqlite-vec = "0.1"` with bundled feature in Cargo.toml |
| Embedding timeout | Network or upstream issue | Increase `--embedding-timeout` |
| FTS5 returns no results | Wrong tenant_id or empty index | Check tenant_id matches what was used during distillation |
| Vector dimension mismatch | Mismatch between config and stored data | Recreate the DB with matching `--vector-dim` |
