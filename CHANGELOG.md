# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed

- **Conflict resolution now actually dedupes.** The existing memory's
  embedding was never loaded from the store (it was incorrectly set to the
  candidate's own vector), so cosine similarity was meaningless and duplicate
  memories were never replaced. `row_to_experience` / `search_by_vector` now
  hydrate the real vector via a new `ExperienceRepository::get_vector` method,
  and `phase_resolve_conflicts` compares against it. Added a regression test
  (`distill_replaces_duplicate_by_vector`) that fails without the fix.
- **FTS5 query injection hardened.** User-supplied keywords are now escaped
  before being placed in an FTS5 `MATCH` expression (`store::fts5_query`),
  so special characters (e.g. `"`, `(`, `:`), `*`) no longer raise a syntax
  error that fails the whole search. The `LIKE` fallback still handles the
  rest.
- **Compiler no longer hardcodes the tenant.** `ConversationCompiler::compile`
  now takes a `tenant_id` argument and uses it for distilled `Knowledge`
  records instead of `"default"`. `memory_compile` passes the caller's tenant.
- **Classifier can now produce `Skill` and `Experience` memories.** Added
  keyword groups for those two `MemoryType`s so the distiller/classifier can
  emit them (previously only `Knowledge`/`Preference`/`Interaction`/`Profile`
  could be produced).
- **Embedding failures are non-fatal.** A failed embedding call no longer
  aborts the entire distillation round; the affected memory keeps an empty
  vector and falls back to keyword-based conflict resolution.
- **`tools/call` validates required arguments.** The MCP server now checks
  `required` fields from each tool's input schema and returns
  `invalid params` (-32602) instead of letting the handler panic.
- **stdio `recv` no longer blocks the tokio worker.** `StdioTransport::recv`
  reads stdin inside `tokio::task::spawn_blocking`.
- **`Config::from_env` validates its result.** It now returns `Result<Self>`
  and runs `validate()`; invalid environment configuration surfaces as an
  error rather than a silently broken server.

### Changed

- Removed the dead `--sse-addr` / `MEMORY_SSE_ADDR` option. The server only
  speaks stdio; an HTTP/SSE transport can be added as a follow-up if needed.
- `Compile` outputs are now tenant-scoped.

### Docs

- Fixed module/tool-count documentation that claimed 5 MCP tools (there are
  6: `memory_distill`, `memory_compile`, `memory_search`, `memory_store`,
  `memory_feedback`, `memory_stats`).
- Clarified capacity control caps `Knowledge` memories per tenant.

## [0.1.0] - 2026-07-xx

- Initial release: 8-stage distillation pipeline, sqlite-vec storage, FTS5 /
  BM25 keyword retrieval, and the six `memory_*` MCP tools.
