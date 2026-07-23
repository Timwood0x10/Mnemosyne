# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **distiller**: `phase_resolve_conflicts` no longer fetches-then-discards the
  existing-memory list. Vector path now rehydrates the existing record's
  embedding before cosine comparison so conflicts are actually detected.
- **distiller**: added FNV-1a content-hash dedup fallback when embeddings are
  disabled (`embedding_provider = none`). Previously, duplicate knowledge
  memories accumulated unbounded because the resolver was a no-op without
  vectors.
- **retrieval**: `hybrid_search` and `vector_search` now report real cosine
  similarity (`1.0 - sqlite_vec_distance`) as `semantic_score`, instead of a
  rank-decay approximation (`1/(1+rank)`) or a hardcoded `1.0`. The weighted
  fusion formula in the README now matches the code.
- **store**: `Experience` gained a `distance: f64` field populated by
  `search_by_vector`, so callers no longer have to re-rank results blindly.
- **main**: `memory_compile` `distill` default aligned to `false` in both the
  handler code and the MCP JSON schema. The two previously disagreed.
- **main**: decisions persisted by `memory_compile` now pass through the
  noise + security filters and are content-deduplicated against existing
  knowledge memories, instead of bypassing the pipeline gates.
- **distiller**: phase numbering in `distill()` renumbered 1–8 to match the
  README's "8-stage pipeline" description (was 1,2,3,4,5,5,6,9,8).
- **distiller**: `tenant_locks` replaced with a bounded LRU map
  (`LruTenantLocks`, cap 1024) to stop the slow memory leak under
  multi-tenant workloads.
- **distiller**: `compress_pair` no longer strips the Chinese particle 呢,
  which was mangling legitimate questions ending in 呢 (e.g. "怎么办呢" →
  "怎么办").
- **filter**: `CHATTER_PHRASES` matching is now boundary-aware via
  `matches_chatter_prefix`. `"hi there"` is still chatter, but
  `"hi, how do I parse JSON?"` is no longer dropped as noise.
- **transport**: `StdioTransport::recv` comment corrected — the blocking
  `read_line` on a tokio worker is acknowledged as a stall risk, not falsely
  justified.

### Added

- `CONTRIBUTING.md` — dev setup, branch/PR conventions, the single-file
  ≤1000 rule, the English-comments rule, and the testing standards excerpted
  from `plan/rules/rules.md`.
- `examples/claude-desktop-config.json` — a working MCP client config for
  Claude Desktop / Cursor pointing at `cargo run --bin memory-mcp -- serve`.
- `Cargo.toml` metadata: `repository`, `homepage`, `keywords`, `categories`.

## [0.1.0] — 2026-07-23

### Added

- Initial release of the Cognitive Memory MCP Server.
- 6 MCP tools: `memory_distill`, `memory_compile`, `memory_search`,
  `memory_store`, `memory_feedback`, `memory_stats`.
- 8-stage distillation pipeline: extract → classify → score → filter →
  embed → resolve conflicts → final top-N → capacity control + sync.
- Conversation compiler with reasoning-chain tracking (goal, module, files,
  open problems, tool invocation arcs).
- Hybrid retrieval: BM25 keyword (FTS5 + CJK LIKE fallback) + sqlite-vec
  cosine vector search + weighted fusion.
- `SQLiteVecStore` backend with tenant isolation, per-type TTL, and
  per-tenant capacity control.
- Pluggable embedding service: `NullEmbedder` (keyword-only mode),
  `RemoteEmbedder` (HTTP, works with OpenAI / Ollama-style endpoints).
- Noise + security filters reject chatter, too-short/long messages, and
  obvious secrets (API keys, tokens, private keys).
- 142 unit tests + 3 doctests, all passing. Clippy clean.
- Apache-2.0 license.
- Dual-language README (English + Chinese).

[Unreleased]: https://github.com/TimWood/memory_distill/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/TimWood/memory_distill/releases/tag/v0.1.0
