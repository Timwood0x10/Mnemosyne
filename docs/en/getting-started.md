# Getting Started

## Prerequisites

- **Rust toolchain** (edition 2024): Install via [rustup](https://rustup.rs/)
- **SQLite**: Bundled automatically (the `bundled` feature of `rusqlite`)
- **No API keys required** for keyword-only mode

## Installation

### From Source

```bash
git clone https://github.com/TimWood/memory_distill.git
cd memory_distill
cargo build --release
```

The binary is placed at `target/release/memory-mcp`.

### From Cargo (when published)

```bash
cargo install memory_distill
```

## Quick Start: Zero-Config Mode

Run the server with no external dependencies — just SQLite:

```bash
cargo run --bin memory-mcp -- \
  --embedding-provider none \
  --retrieval-mode keyword \
  --db-path ./my-memories.db
```

The server starts in **stdio MCP mode**, listening for JSON-RPC 2.0 messages on stdin/stdout. It is ready to receive tool calls immediately.

> **What happens here**: SQLite FTS5 handles all search. No API calls, no network, no vector database. Zero ongoing cost.

## Verifying It Works

With the server running, send it a `tools/list` request (from another terminal):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | nc -w1 localhost 8080
```

In stdio mode, you would typically connect through an MCP client. A quick test with a Python helper:

```bash
# Start the server in background, capture its output
cargo run --bin memory-mcp -- --db-path /tmp/test.db > /tmp/mcp-out &
MCP_PID=$!

# Send a tools/list request
printf '{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n' > /dev/stdin
```

## Distill Your First Memory

Connect an MCP client and call the `memory_distill` tool:

```json
{
  "conversation_id": "session-1",
  "messages": [
    {"role": "user", "content": "How do I parse JSON in Rust?"},
    {"role": "assistant", "content": "Use serde_json::from_str with a typed struct."}
  ]
}
```

The server returns metrics: how many experiences were extracted, classified, stored, and whether any conflicts were resolved.

## Search Your Memories

```json
{
  "query": "parse JSON Rust",
  "limit": 5
}
```

The response contains ranked memories with relevance scores.

## Next Steps: Enable Embeddings (Optional)

### With OpenAI

```bash
MEMORY_OPENAI_API_KEY=sk-... cargo run --bin memory-mcp -- \
  --embedding-provider openai \
  --vector-dim 768 \
  --retrieval-mode hybrid
```

### With Ollama (local)

First ensure Ollama is running with an embedding model:

```bash
ollama pull nomic-embed-text
```

Then configure the server (Ollama endpoint and related settings can be configured via env vars — see [configuration.md](configuration.md)).

## Claude Desktop Integration

Add to your `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "memory": {
      "command": "/path/to/memory-mcp",
      "args": [
        "--db-path", "/path/to/memory.db",
        "--retrieval-mode", "keyword"
      ]
    }
  }
}
```

Example config from the project:

```json
{
  "mcpServers": {
    "memory": {
      "command": "cargo",
      "args": [
        "run",
        "--bin", "memory-mcp",
        "--",
        "--db-path", "./memory.db",
        "--retrieval-mode", "keyword",
        "--embedding-provider", "none"
      ],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

## Development Commands

```bash
make check      # Run clippy + cargo check
make test       # Run 142 unit tests + 3 doctests
make run        # Start the server in stdio MCP mode
make build      # Build in release mode
```

## What's Next?

- Learn about the [system architecture](architecture.md)
- Explore the [distillation pipeline](distillation-pipeline.md) in detail
- See all [MCP tools](mcp-tools.md) and their APIs
- Configure the server for your environment: [configuration.md](configuration.md)
