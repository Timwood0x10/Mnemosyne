# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.2] - 2026-08-10

### Fixed

- **Prebuilt binaries no longer hardcode the build machine's source path.**
  `env!("CARGO_MANIFEST_DIR")` was baked into every release, so a binary
  built on CI panicked with `Failed to load core lexicon from
  config/dictionary.json` (`FileLoad { path: "/Users/runner/work/..." }`) on
  every other machine. Resource paths are now resolved at runtime
  (`resolve_resource_path`): `MNEMOSYNE_HOME` env override → current working
  directory → executable's directory, in that order.
- **A missing lexicon no longer crashes the process.** The global lexicon
  registry previously panicked on first use when `config/dictionary.json` was
  absent; it now degrades to an empty registry with a warning (same fail-soft
  pattern as the dictionary loader).
- **`generalize_compile` extracted document titles as fake `person`
  entities.** Auto-generated titles (`generalize-1786331280`) and filenames
  were materialized as person objects while the actual people inside the text
  (张三/李四/Alice/Bob) were never discovered — the knowledge graph was
  unusable. Title entities are now only created when the title looks like a
  real name, and corpus discovery was added/extended:
  - English Capitalized person names (`Alice met Bob`) are extracted.
  - Vernacular Chinese dialogue verbs (`说/说道/答道`) beyond the classic
    novel list (`曰/道`) now surface speakers.
  - Overlapping verbs in one run (`说道` matching 说/道/说道) no longer
    double-count frequencies.
- **`memory_compile` produced zero facts for ordinary conversations.** Only
  12 hardcoded marker words were matched, so "我很焦虑，压力很大" compiled no
  facts and the facts table stayed empty. The marker table was moved out of
  the binary into configurable JSON (see below) and expanded to cover
  emotions, preferences, plans, wants, beliefs, difficulties, life events,
  modern vernacular, and internet slang.
- **Duplicate-fact inflation.** One message matching several same-action
  markers (失眠+加班+压力+好累 → four `feel`) emitted four near-identical
  facts, polluting the cognitive snapshot. Observations are now deduplicated
  per action per message while distinct actions are preserved.
- **Companion relationships never advanced.** `relationship_update` only
  counted user-message emotions, so an assistant-heavy warm dialogue
  ("我很开心能认识你" / user replies "嗯嗯") left intimacy pinned at 0.0
  forever. The agent's own emotional statements now move the relationship
  with a lighter weight (user emotions remain the primary driver).

### Changed

- **One config root instead of ten environment variables.** The scattered
  `DICTIONARY_PATH` / `FACTION_MAP_PATH` / `DECAY_CONFIG_PATH` /
  `RELATION_RULES_PATH` / `PERSONA_CARDS_PATH` / `PERSONA_PROTOTYPES_PATH` /
  `DOMAIN_PROFILES_PATH` / `EMOTION_LEXICON_PATH` / `ANCHOR_SEEDS_PATH` /
  `NAME_VALIDATION_PATH` overrides were removed in favor of a single
  `MNEMOSYNE_HOME` root directory, auto-detected when unset.
- **Observation markers are now data, not code.** The marker table moved from
  a hardcoded array into two shipped, user-editable JSON files —
  `markers_zh.json` and `markers_en.json` — with a built-in fallback table
  when the config is absent.
- **Default HTTP listen port changed 8080 → 5609** (avoiding a common
  conflict); `--http-addr` still overrides it.
- **Release artifacts now include the marker word lists.** Each platform
  binary ships alongside `markers_zh.json` / `markers_en.json`, so a download
  placed in one directory works out of the box and is customizable.

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

## [0.1.3] - 2026-09-22

### Added

- **Cognitive state history.** `StateEngine::aggregate_intervals`
  projects an entity's facts into per-dimension `StateEvolution`s — time-ordered
  `StateInterval`s with their evidence anchors plus deterministic
  `StateTransition`s (`gradual_change` / `stance_flip` /
  `behavioral_confirmation`). ADD-only: facts are never removed, state is always
  recomputable, and a change without a definite signal is reported as intervals
  only instead of a fabricated transition. Exposed as the `state_timeline` tool.
- **Fact provenance.** Facts carry `confidence`, a three-state
  epistemic `status` (active/superseded/contradicted) and a `derived_from`
  derivation chain; the `fact_provenance` tool audits why a fact is believed.
- **Decision layer write path.** `memory_compile` now compiles explicit
  commitments ("我答应…" / "I promise…") into first-class `Decision` records.
  The commitment utterance is stored as an anchored Event fact first and the
  decision's `because` points at it, so `decision_trace` can walk from a
  decision back to its evidence. Extraction is rule-driven and LLM-free, and is
  deliberately conservative: a bare plan ("我会…") is not a promise.
- `decision_trace` / `decision_search` MCP tools for reading the decision layer.
- **Cognition-layer end-to-end test over the real MCP path.**
  `tests/cognitive_state_e2e.rs` drives `MCPServer::serve` through an in-memory
  `Transport` (`tools/call`), covering `memory_compile` → `state_timeline` /
  `decision_search` → `decision_trace` → `fact_provenance`. Every other
  integration test called a handler directly, which is how hand-crafted payloads
  kept the state-layer defects invisible.
- **The plan's Step 2 acceptance runs on real data.**
  `tests/cognitive_state_e2e.rs::three_state_evolution_chain_carries_its_evidence`
  drives the "宅家 → 想社交 → 第一次参加活动" corpus through the real compiler and
  the real `state_timeline` / `fact_provenance` tools, asserting three validity
  windows, a distinct evidence anchor per state, and the two transitions the data
  can actually prove (`gradual_change`, then `behavioral_confirmation`).
- **Optional `tenant_id` on the four id-addressed read tools.** `state_timeline`,
  `fact_provenance`, `decision_trace` and `decision_search` address their subject
  by a raw id, which says nothing about ownership. When a caller supplies the
  tenant, the subject must belong to it and a mismatch is reported as not-found —
  never as a permission error, which would confirm that the id exists.

### Fixed

- **`state_timeline` returned zero dimensions for every real conversation.**
  Dimensions were selected by a payload field (`preference`, `emotion`, …) that
  no production compiler emits — facts carry only `content`/`negated`/
  `attribution`. Dimensions are now selected by `FactType`, the same filter
  `StateEngine::aggregate` uses, with a payload fallback so distinct key-less
  facts never fold into one interval.
- **Transition endpoints were pinned to the tail pair.** The transition loop
  wrote `len() - 2`/`len() - 1` while `intervals` did not grow, so every
  transition of a dimension with three or more states pointed at the last two.
  Each transition now references its own window index.
- **`set_decision_outcome` could overwrite a recorded outcome.** The
  read-then-write pair was replaced by a single guarded statement
  (`UPDATE … WHERE id = ?2 AND outcome IS NULL`), so a decision can never be
  both fulfilled and violated.
- **`decision_search` did not escape LIKE wildcards and did not clamp `limit`.**
  A query of `%` matched every decision, and an out-of-range `limit` was
  honoured instead of being capped at the schema maximum.
- **`insert_decision` never validated its input.** `validate_decision` existed
  but was only called from tests; it is now enforced on the write path.
- **Decay silently rewrote epistemic confidence.** `confidence` and the decay
  score shared the `weight` column, so archiving a stale fact also lowered "how
  much do we believe this?" — the plan requires `confidence ≠ status ≠ decay`.
  `confidence` now owns its column and the decay path only writes
  `weight`/`archived`/`status`-independent state. Databases created before the
  split are migrated in place: when the column is first added it is backfilled
  from `weight`, so accumulated confidence survives.
- **Cognitive-state transitions were unreachable on production data.**
  `gradual_change` required a `keyword` payload field while the comparison text
  came from `content`, and no compiler emits both — so the plan's own example
  ("喜欢独处 → 开始想社交 → 喜欢热闹") could never be reported. The comparison
  text now falls back through `content` → `object` → `keyword` → `action`, and a
  value change counts as gradual when the two states share a topic
  (`keyword`/`topic`/`preference`/`action`).
- **A user's negated statements were discarded at compile time.** "我不喜欢应酬"
  produced no fact at all, so the engine could not represent a negative stance and
  `stance_flip` was structurally impossible for a user entity — negated facts only
  ever existed for the agent's own persona. Negated observations now keep their
  fact with `negated: true`; only `Goal` stays suppressed
  (ELITE_LEXICON_PLAN §13.3), and the affirmative side declares `negated: false`
  so both sides of a flip are readable.
- **One state could be reported as an interval per observation.** Companion themes
  carry `{keyword, occurrences, samples}`; folding on the whole payload meant the
  ever-growing `occurrences` counter made every compile look like a new state. The
  dimension key tables now lead with stable fields.
- **`memory_decay` down-weighted almost the whole store on its first pass.** An
  absent `access_count` was scored as "never accessed"; unknown access history is
  now neutral, and only an explicit `0` decays. The documented role is corrected
  too: decay is a curation signal exposed through `decay_status` / `list_archived`
  — it never hides a fact from a read path.
- **A legacy database with a NULL `weight` could not be opened at all.** The
  `confidence` migration copied `weight` verbatim into a `NOT NULL` column, so a
  single NULL row made `SqliteFactStore::open` fail; it now uses
  `COALESCE(weight, 1.0)`.
- **Commitment extraction could record the opposite of what was said.**
  "compromise" matched the `promise` marker and "I will not help you" became a
  commitment. ASCII markers now have to start on a word boundary, and a negation
  cue in the same clause (or immediately after the marker) cancels the decision.
- **A rejected decision left half a compilation behind.** Facts were written before
  decisions were validated, so an error was returned while the facts — and
  sometimes an orphan anchor — stayed behind, and a retry duplicated them. The
  compile path now prepares everything, validates first, and commits facts,
  anchors and decisions in ONE transaction.
- **`state_timeline` published a fabricated evidence link.** An interval built from
  a fact without an id reported `fact_ids: [0]`, an id no row can satisfy.
- **Compiled facts never registered their evidence anchor.** The compilers keep the
  original utterance in `payload.evidence` and the fact was written with
  `evidence_id = NULL`, so nothing ever wrote the `evidence` table in production:
  `fact_provenance`'s "why do we believe this?" answered `null` for every real fact
  and `state_timeline` intervals never carried an `evidence_ids` entry, even though
  the plan requires each state to carry its evidence. The write path now registers
  the anchor inside the same transaction — one row per utterance, shared by the
  facts it produced, with `doc_id: 0` ("not from a document") stored as NULL.
- **Stale documentation.** The `confidence` field still described itself as
  "mapped from the legacy `weight` column (decay down-weights it)" after the split;
  `plan/cognitive-state-v03.md` and `plan/external-knowledge-plan.md` were dangling
  links; and the tool's dimension list was a hand-maintained copy that would have
  rejected any dimension added to the engine.

### Changed

- **Large modules split to satisfy the one-file-per-1000-lines rule.**
  `fact_store`, `cognition`, `store`, `retrieval`, `conversation_compiler`,
  `character`, `distiller`, `lexicon`, `knowledge/store` and
  `compiler/name_validation` were decomposed into focused submodules, together
  with the binary's `memory_*` / `character_*` tool handlers. `fact_store` and
  `knowledge/store` also gained their own test modules. `FactType::as_str`
  replaced three duplicated `fact_type_name` mappings.
- **Suppression and stderr cleanups.** All `#[allow(...)]` attributes were removed
  (a `field_reassign_with_default` on the config tests, a stale
  `too_many_arguments`, and a `dead_code` JSON field that serde ignores anyway),
  and library warnings now go through `tracing` instead of `eprintln!`. The
  transition tests moved to `tests/state_transitions.rs`, which brings
  `src/state.rs` back under the 1000-line rule.

### Docs

- `docs/{en,zh}/mcp-tools.md`: the overview table now covers `state_timeline`,
  `fact_provenance`, `decision_trace` and `decision_search`, each with an input
  schema and an example; the stale "10 MCP tools" claim is replaced by a pointer
  to the authoritative list in `README*.md`.
- `README.md` / `README.zh.md`: the Testing and Test Corpora sections now
  describe the self-contained suite (no fixture dependency) instead of
  referencing test targets and corpora that no longer exist.

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
