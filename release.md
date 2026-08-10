# Mnemosyne v0.1.1

**Mnemosyne** (Greek: Μνημοσύνη) — the Memory Distillation Engine.

An **LLM-free knowledge distillation engine + MCP server** that compiles
narrative text into a world model and conversations into cognitive facts.
Named after the Greek goddess of memory: memory never dies, is always
reconstructable, and persists across sessions. Companion AIs use it to keep a
consistent persona without an LLM in the loop.

> **Facts come from compilation, not guesswork.**
> **State comes from events, not prompts.**
> **Long-term cognition comes from the model, not context window length.**

## Supported platforms

| Platform | Architecture | Binary |
|---|---|---|
| macOS | arm64 (Apple Silicon) | `mnemosyne-aarch64-apple-darwin` |
| macOS | x86_64 (Intel) | `mnemosyne-x86_64-apple-darwin` |
| Linux | arm64 | `mnemosyne-aarch64-unknown-linux-gnu` |
| Linux | x86_64 | `mnemosyne-x86_64-unknown-linux-gnu` |
| Windows | x86_64 | `mnemosyne-x86_64-pc-windows-msvc.exe` |

All binaries are single-file, statically configured (SQLite-only, no API key
required), and ~6–8 MB after release optimization.

## Quick start

```bash
# 1. Local stdio mode (IDE / Claude Desktop / Cursor integration)
./mnemosyne serve

# 2. Remote HTTP+SSE mode (token required; session isolation per client)
./mnemosyne --transport http --http-addr 0.0.0.0:5609 --http-token <your-token> serve
```

### IDE integration

```jsonc
// .mcp.json
{
  "mcpServers": {
    "mnemosyne": {
      "command": "/path/to/mnemosyne",
      "args": ["serve"]
    }
  }
}
```

## Highlights

- **Dual MCP transport** — stdio for local IDEs; HTTP+SSE for remote
  deployments with per-session `x-mcp-session-id` isolation, mandatory
  `--http-token` auth (constant-time comparison), and file-allowlist sandbox.
- **33 MCP tools** — `memory_compile`, `generalize_compile`,
  `persona_check`, `persona_inject`, `inspect_entity`, `timeline`,
  `relation_graph`, `search_graph`, `trace_path`, `memory_decay`,
  `memory_export/import`, `knowledge_attach/ingest`, and more.
- **LLM-free semantic similarity** — FTS5 keyword search, HNSW / brute-force
  cosine vector retrieval, character-bigram Jaccard name matching, and hybrid
  retrieval. Zero API-key dependency, fully reproducible.
- **Knowledge graph** — narrative text → documents / entities / events /
  relations / evidence with traceable evidence chains (EvidenceRef).
- **Companion-AI persona guard** — `persona_check` / `persona_inject` /
  `relationship_update` keep a companion AI's persona consistent; memory
  decay never deletes, preserving evolution history.
- **Deterministic pipeline** — Aho-Corasick verb matching, rule-driven event
  extraction, 8-stage distillation. No LLM guessing anywhere.

## Changelog (v0.1.1)

- **Rebrand to Mnemosyne** (formerly LoreScope): package, binary, docs,
  Makefile, repository.
- **MCP hardening**: HTTP requires `--http-token` with constant-time compare;
  per-session SSE isolation (`x-mcp-session-id`); `memory_export/import` path
  allowlist.
- **Faster CI/release**: new `[profile.ci]` (thin LTO + 16 units) for builds;
  release runs after CI succeeds, builds all 5 platforms, verifies assets
  before publishing, version tag from `release.md`.
- **Self-contained tests**: all corpus-dependent tests removed (corpus is
  gitignored); suite runs fully on CI with synthetic data only.
- **Wire fix**: `ToolDefinition.input_schema` now serializes as `inputSchema`
  (MCP camelCase contract).

## Docs

- [README](README.md) / [README.zh.md](README.zh.md)
- [Architecture](docs/en/architecture.md) / [Architecture (zh)](docs/zh/architecture.md)
- [Module docs](docs/en/compiler.md) / [Module docs (zh)](docs/zh/compiler.md)

## License

Apache-2.0
