# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.1] - 2026-08-08

### Changed

- **Rebrand to Mnemosyne.** Package/binary renamed from `LoreScope`/`lore-scope`
  to `Mnemosyne`/`mnemosyne` (Cargo.toml, all `lore_scope::` imports, README,
  docs, Makefile). The GitHub repository is `Timwood0x10/Mnemosyne`.
- **MCP hardening for external serving.** HTTP transport now requires
  `--http-token` (refuses to start without it) and compares tokens in
  constant time; per-session SSE isolation via `x-mcp-session-id` so concurrent
  clients never receive each other's responses; `memory_export/import` `path`
  restricted to the `exports/` allowlist (rejects absolute paths and `..`
  traversal).
- **Faster release pipeline.** New `[profile.ci]` (thin LTO + 16 codegen
  units) used by CI builds — fat LTO + 1 unit was the dominant compile cost on
  every platform. `release.yml` now runs AFTER CI succeeds (`workflow_run`),
  builds 5 platform binaries (macOS arm64/x86_64, Linux arm64/x86_64, Windows
  x86_64), verifies all 5 assets exist before publishing, and derives the
  version tag from `release.md`.
- **CI self-contained.** Removed all corpus-dependent tests (corpus/ is
  gitignored and never present in CI); the suite now uses only synthetic
  corpora generated in-test. `ci.yml` runs `make check && make test` on `dev`.

### Fixed

- **Hardcoded corpus paths removed.** All tests that read `corpus/*.txt|json`
  (or shared `/tmp/*.db` fixtures) were deleted rather than skipped, so CI
  never fails with `NotFound` on a missing fixture.
- **`ToolDefinition.input_schema` serializes as `inputSchema`.** The MCP wire
  contract requires camelCase; strict clients previously could not read the
  tool schema.

### Security

- Constant-time bearer-token comparison in the HTTP transport (no timing side
  channel); file-allowlist for memory transfer tools; FTS5 query injection
  hardening (see earlier entry).

## [Unreleased]

### Changed

- **V7 wiring sprint: removed dead compiler code.** Seven TODO-marked
  modules under `src/compiler/` (`alias.rs` / `pronoun.rs` / `merge.rs` /
  `relation.rs` / `inference.rs` / `entity/conversation.rs` / `entity/regex.rs`)
  were never declared in `mod.rs` and had zero references; they are deleted,
  together with the corresponding `ConversationProvider` / `RegexProvider`
  exports in `entity/mod.rs`.
- **NovelProvider wired into the production compile path.**
  `compile_source` (the `generalize_compile` tool) now registers known
  characters from the novel dictionary that actually appear in the text (by
  name or alias), fixing review NEW-C20 (profile JSON `entities` all empty +
  NovelProvider never instantiated); characters are stored with a
  `source=novel_dictionary` marker and alias attributes, and V7 world entities
  are synced as well.
- Added end-to-end verification:
  `tests/mcp_corpus_full_loop.rs::generalize_then_inspect_entity_e2e` walks
  the real MCP `tools/call` path (generalize_compile → inspect_entity),
  confirming the V7 compile → graph → query chain holds.
- **Transactional migration (review H6).** `Migrator::migrate` now wraps the
  whole V1→general migration in a single SQLite transaction
  (`SQLiteKnowledgeStore::begin/commit/rollback_transaction`); a mid-way
  failure rolls everything back instead of leaving a half-migrated database
  (documents present, chapters missing; dangling edges, etc.). Added three
  store-level transaction tests (commit persists / rollback discards /
  multi-row rollback).
- **Full-corpus acceptance regression.** Added
  `tests/generalize_corpus_regression.rs`: runs `compile_source` →
  `inspect_entity` over 7 novel corpora (Romance of the Three Kingdoms,
  Water Margin, Dream of the Red Chamber, Journey to the West, Investiture
  of the Gods, Love in a Fallen City, Pride and Prejudice) plus 3 dialog
  corpora, all green (Sanguo 3644 objects / 55763 edges; Honglou and
  Fengshen detect story events).

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
