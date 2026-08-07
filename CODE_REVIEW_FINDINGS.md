# LoreScope — Deep Code Review Findings (v2)

**Date:** 2026-07-29 (second pass)
**Method:** 4 parallel review agents (compiler, knowledge, MCP/CLI, ingest) + aggregation
**Baseline:** `make check && make fmt` pass with 0 errors / 0 warnings; 280 tests pass
**Total findings:** 59 (5 CRITICAL, 17 HIGH, 22 MEDIUM, 15 LOW) — deduplicated across reviewers

---

## Headline

The previous pass fixed 9 of 11 CRITICAL/HIGH issues (C3, C7, C8, C9, H1, H4, H5-partial, H7, H8, H9). This pass verified those fixes are correct and found **22 new issues**, including 3 new CRITICALs and 11 new HIGHs. The most impactful new findings:

1. **NEW-C21** — Overlapping chunks produce duplicate sentences → duplicate events (compiler)
2. **NEW-K1** — `inspect_entity` lifecycle uses `chapter_id` (surrogate key) instead of `chapter_no` — wrong numbers for any document after the first
3. **NEW-K2/NEW-H1** — `correct_relation` extracts `doc` param but never uses it for entity resolution (cross-document mutation risk)
4. **NEW-H24** — `discover_entity_name` skips `。` (sentence boundary) producing garbage like "说话刘备"
5. **NEW-I3** — Death detection false positives ("刘备闻关羽死" marks 刘备 as dead) + last-chapter-wins overwrites

---

## Verification of Previous Fixes (v1 → v2)

| ID | Was | Status | Evidence |
|----|-----|--------|----------|
| C3 | byte-length bug in discover_entity_name | ✅ FIXED | `profile.rs:253` uses `result.chars().count()` |
| C7 | tools/call missing name crashes connection | ✅ FIXED | `server.rs:267-287` returns -32602 |
| C8 | correct_relation never updates DB | ✅ FIXED | `knowledge_tools.rs:204-208` calls `update_edge_target` |
| C9 | migrator not idempotent | ✅ FIXED | `migration.rs:134-138` calls `clear_for_document` |
| H1 | JSON parse error terminates connection | ✅ FIXED | `server.rs:173-191` returns -32700 |
| H4 | correct_relation ignores old_target | ✅ FIXED | `knowledge_tools.rs:188-193` filters on full triple |
| H5 | inspect_entity empty profile/lifecycle | ⚠️ PARTIAL | profile + lifecycle populated; `character_arc` still `None` (NEW-K4) |
| H7 | PRAGMA foreign_keys never set | ✅ FIXED | Both stores set `foreign_keys = ON` + `busy_timeout = 5000` |
| H8 | inspect_entity events not deduped | ✅ FIXED | `store.rs:625` uses `HashSet<i64>` |
| H9 | evidence offsets mismatch | ✅ FIXED | `migration.rs:434-438` uses snippet window `[lo, hi]` |
| H13 | build_relations dedup drops all | ✅ FIXED | `extract.rs:270` uses `any()` with no break |
| H18 | chapter heading off-by-one | ✅ FIXED | `extract.rs:67` assigns chapter before processing |
| H19 | parse_chapter_number matches anywhere | ✅ FIXED | `extract.rs:174` requires `第` at start |
| C11 | novel_file_map dead code | ✅ FIXED | Function removed |
| PRAGMA | character.rs used WAL, knowledge.rs didn't | ✅ FIXED | Both stores now use rollback journal consistently |

---

## Still-Present Architectural Issues (tech debt — deferred)

| ID | Sev | Location | Finding |
|----|-----|----------|---------|
| C1 | CRITICAL | `src/compiler/` | Entire module is dead code in production; only 4 test files use it |
| C4 | HIGH | `src/compiler/writer.rs` | Stub — no persistence to KnowledgeStore |
| C5 | HIGH | `src/compiler/pipeline.rs` | Stub — no compile() entry point |
| C6 | HIGH | `src/compiler/inference.rs` | Stub — Layer 5 missing (+ relation/merge/pronoun/alias.rs) |
| C10 | CRITICAL | `src/storage/schema.rs:27` | `WORLD_SCHEMA` (V7 tables) defined but never executed |
| H6 | HIGH | `src/knowledge/migration.rs` | ✅ FIXED (V7 wiring sprint) — `Migrator::migrate` now wraps the whole run in one SQLite transaction (`begin_transaction` / `commit_transaction` / `rollback_transaction` on `SQLiteKnowledgeStore`); a mid-run failure rolls back every row instead of leaving a half-migrated DB. Tests: `transaction_commit_persists_writes`, `transaction_rollback_discards_writes`, `transaction_rollback_undoes_multi_row_writes` |
| H10 | HIGH | `src/compiler/extract.rs:254` | `scan_mentions` hardcodes `sentence_id: 0` |
| H11 | HIGH | `src/compiler/entity/registry.rs:97` | `assign_ids` never called → `Mention.entity_id` always `None` |
| H12 | MEDIUM | `src/compiler/entity/mod.rs:36` | `EntityEngine` (Aho-Corasick) unused; extract.rs reimplements |
| H14 | CRITICAL | `src/compiler/timeline.rs:269` | `build_timeline` output never merged with `ctx.relations` or persisted |
| H15/16 | HIGH | `src/compiler/entity/json_provider.rs` | JSON dialog/hostile/friendly/faction triggers loaded but never consumed |
| H17 | MEDIUM | `src/compiler/entity/registry.rs:73` | Alias collisions silently overwrite |
| H20 | HIGH | `src/ingest/mod.rs:575,594,613,744` | Pipeline silently swallows DB errors with `.ok()` |
| H21 | HIGH | `src/ingest/corpus.rs:182` | Sync `std::fs::read_to_string` in async `distill_novel` |
| H2 | HIGH | `src/main.rs:642` vs docs | Tool registered as `lore_scope` but docs say `memory_distill` |
| H3 | HIGH | `src/main.rs:132-158` | `memory_feedback` stub — logs but never persists; `store` field dead |

---

## New Findings (v2)

### CRITICAL

| ID | Location | Finding |
|----|----------|---------|
| NEW-C20 | `config/entity_profiles/*.json` | All 4 JSON profiles ship `"entities": []` → empty entity dictionary; `NovelProvider` never instantiated |
| NEW-C21 | `src/compiler/chunk.rs:60` + `sentence.rs:97` | Overlapping chunks produce duplicate sentences → duplicate events; `split_all` has no dedup |
| NEW-I1 | `src/ingest/characters.rs:974`, `corpus.rs:12`, `mod.rs:134` | Pipeline hardcoded to 4 novels + ~250 `CharacterDef` structs; violates "general template" goal |

### HIGH

| ID | Location | Finding |
|----|----------|---------|
| NEW-H22 | `src/compiler/timeline.rs:337` | `build_timeline` clobbers `valid_from` with latest timestamp when relation type unchanged |
| NEW-H23 | `src/compiler/timeline.rs:320,344` | `build_timeline` emits duplicate reverse-direction relations (4 rows per transition) |
| NEW-H24 | `src/compiler/profile.rs:262` | `discover_entity_name` skips `。` (sentence boundary) → garbage like "说话刘备" |
| NEW-H25 | `src/compiler/profile.rs:174` | `find_entity_in_text` non-deterministic for same-length aliases (HashMap order) |
| NEW-H26 | `src/compiler/sentence.rs:52` | Sentence `start_offset`/`end_offset` point to untrimmed boundaries while `text` is trimmed |
| NEW-H27 | `src/compiler/timeline.rs:127` | Raw-text personality markers hardcoded to `chapter: 0`, corrupting character-arc detection |
| NEW-K1 | `src/knowledge/store.rs:677-678` | `inspect_entity` lifecycle uses `chapter_id` (surrogate) instead of `chapter_no` — wrong for 2nd+ document |
| NEW-K2 | `src/mcp/knowledge_tools.rs:170-181` | `correct_relation` extracts `doc` param but passes `None` to all `find_object_by_name` calls |
| NEW-K3 | `src/storage/schema.rs:60` | `WORLD_SCHEMA`'s `entity_profiles.evidence_id` FK references `evidence(id)` from `KNOWLEDGE_SCHEMA` — latent cross-schema FK bug |
| NEW-I2 | `src/faction.rs:23` | `unsafe { std::mem::transmute }` for lifetime extension — fragile, could become UB |
| NEW-I3 | `src/ingest/mod.rs:354-369` | Death detection: 100-byte window after name matches OTHER characters' deaths + "last chapter wins" overwrites |

### MEDIUM

| ID | Location | Finding |
|----|----------|---------|
| NEW-K4 | `src/knowledge/store.rs:728` | `character_arc` always `None` — un-wired field |
| NEW-K5 | `src/knowledge/store.rs:291` | `clear_all` is dead code (zero callers) |
| NEW-K6 | `src/knowledge/store.rs:315` | `format!` interpolation in `clear_for_document` SQL (safe-by-type but bad pattern) |
| NEW-K7 | `src/knowledge/store.rs:660,815` | N+1 queries in `inspect_entity` (per-edge evidence) and `relation_graph` (edges fetched twice) |
| NEW-K8 | `src/knowledge/store.rs:679` | `death_chapter` keyword check brittle, only checks event name not description |
| NEW-K9 | `src/knowledge/store.rs:271` | `set_foreign_keys_enabled(false)` affects all concurrent users of shared connection |
| NEW-M1 | `src/knowledge/store.rs:491` | `update_edge_target` discards affected-row count; `changed` counter can overstate |
| NEW-M2 | `src/mcp/knowledge_tools.rs:306` | `correct_relation` description says "reports" but actually applies the correction |
| NEW-M3 | `src/main.rs:84` | `memory_search` does not clamp `limit` (can pass 1000000) |
| NEW-M4 | `src/main.rs:369,820` | `CharacterSearchTool` handler default 50 but schema documents 10 |
| NEW-M5 | `src/main.rs:60` vs docs | `lore_scope` response shape doesn't match docs |
| NEW-M6 | docs + `lib.rs:8` | Docs say "10 MCP tools"; server registers 15 (5 knowledge tools undocumented) |
| NEW-M8 | `src/mcp/server.rs:176` | Parse-error detection relies on brittle `msg.starts_with("parse:")` string match |
| NEW-M11 | `src/mcp/types.rs:55` | `ToolCallResult.is_error` serializes as `is_error` but handler writes `isError` |
| NEW-I4 | `src/ingest/mod.rs:180` | Single-char shortnames (`飞`/`云`) not wired into dialog relation extraction |
| NEW-I5 | `src/ingest/mod.rs:738` | `source_type` always `CoOccurrence`; `DialogChain` variant never used |
| NEW-I6 | `src/ingest/mod.rs:481` | Directional dialog relations stored as `bidirections: true` |
| NEW-M28 | `src/compiler/extract.rs:111` | Overlapping verbs create duplicate events |
| NEW-M29 | `src/compiler/extract.rs:121` | Multi-verb sentences misassign subjects/objects |
| NEW-M30 | `src/compiler/timeline.rs:360` | `infer_relation_type` ignores object role, mislabels witness pairs |
| NEW-M32 | `src/compiler/mod.rs:142,163` | `ctx.mentions`/`CompileStats`/`EvidenceSlice` dead code |
| NEW-M33 | `src/compiler/profile.rs:196` | 500+ entry NOISE list with duplicates |

### LOW

| ID | Location | Finding |
|----|----------|---------|
| NEW-K10 | `schema.rs:115` vs `migration.rs:507` | Timestamp timezone inconsistency (localtime default vs UTC insert) |
| NEW-K11 | `src/knowledge/store.rs:895` | `search_evidence` is full table scan (no FTS) |
| NEW-K12 | `src/knowledge/store.rs:781` | `entity_timeline` defaults `chapter` to 0 when `valid_from` is NULL |
| NEW-L34 | `src/compiler/extract.rs:99` | Dialog event title hardcoded as "X曰" regardless of marker |
| NEW-L35 | `src/compiler/extract.rs:103` | Event description = text before verb, not the sentence |
| NEW-L36 | `src/compiler/extract.rs:207` | `chinese_to_int` missing 万 (10000) unit |
| NEW-L37 | `src/compiler/chunk.rs:134` | `char_to_byte_offset` is O(n) per call, making chunk::plan O(n²) |
| NEW-L38 | `src/compiler/chunk.rs:96` | `overlap_before`/`overlap_after` metadata computed but never consumed |
| NEW-L40 | `src/conversation_compiler.rs:165` | Tool-gapped replies misflagged as unanswered |
| NEW-I7 | `src/ingest/relation.rs:129` | Dead code — `DIALOG_ADDRESS_RULES` const duplicates JSON config |
| NEW-I9 | `src/ingest/relation.rs` | `is_poem_prefix` too broad — any `诗`/`词` within 20 bytes skips dialog |
| NEW-I10 | `src/ingest/mod.rs` | Debug `eprintln!` instrumentation in production code (6 sites) |
| NEW-M14 | `src/mcp/server.rs:225` | `protocolVersion` hardcoded to "2024-11-05" |
| NEW-M15 | `src/main.rs:64` | main.rs tools don't set `mimeType` on content blocks |
| NEW-M16 | `src/mcp/server.rs:336` | `err_text.clone()` unnecessary clone |

---

## Un-wired Pipelines (consolidated v2)

| Pipeline | Status | Location |
|----------|--------|----------|
| V7 compiler → production | Dead code | C1 |
| compiler `writer.rs` → KnowledgeStore | Stub | C4 |
| compiler `pipeline.rs` (orchestration) | Stub | C5 |
| compiler `inference.rs` (Layer 5) | Stub | C6 |
| `WORLD_SCHEMA` V7 tables → init() | Never executed | C10, NEW-K3 |
| `build_timeline` → `ctx.relations` | Separate vec, never merged | H14 |
| JSON verb lists → TimelineConfig/FactionTracker | Loaded, never consumed | H15/H16 |
| `EntityProfileEntry.character_arc` | Always None | NEW-K4 |
| `clear_all` method | Dead code | NEW-K5 |
| `assign_ids` → `Mention.entity_id` | Never called | H11 |
| `EntityEngine` (Aho-Corasick scanner) | Defined, never used | H12 |
| `ctx.mentions` field | Never populated | NEW-M32 |
| `CompileStats` / `EvidenceSlice` | Defined, never instantiated | NEW-M32 |
| `NovelProvider` (has real data) | Never registered | NEW-C20 |
| `memory_feedback` → persistence | Logs only | H3 |
| `correct_relation` `doc` param → entity resolution | Extracted, never passed | NEW-K2 |
| `RelationSource::DialogChain` variant | Never used | NEW-I5 |
| `overlap_before`/`overlap_after` metadata | Computed, never consumed | NEW-L38 |

---

## Fix Plan (this pass — safe, high-impact, low-risk)

| Fix | Finding | Risk | Effort |
|-----|---------|------|--------|
| Remove `。` from skip list in discover_entity_name | NEW-H24 | Low | 1 line |
| Add deterministic tie-break sort for same-length aliases | NEW-H25 | Low | 1 line |
| Fix `build_timeline` valid_from clobbering | NEW-H22 | Low | Small |
| Fix `build_timeline` reverse-direction duplicates | NEW-H23 | Low | Small |
| Dedup overlapping-chunk sentences in `split_all` | NEW-C21 | Medium | Small |
| Fix `inspect_entity` lifecycle chapter_id → chapter_no | NEW-K1 | Low | Small |
| Wire `doc` param in `correct_relation` | NEW-K2 | Low | Small |
| Fix death detection: first-occurrence-wins + tighter window | NEW-I3 | Medium | Small |
| Remove `unsafe transmute` in faction.rs | NEW-I2 | Low | Small |
| Add `#[serde(rename = "isError")]` to ToolCallResult | NEW-M11 | Low | 1 line |
| Clamp `memory_search` limit to 200 | NEW-M3 | Low | 1 line |
| Fix CharacterSearchTool default limit mismatch | NEW-M4 | Low | 1 line |
| Update `correct_relation` description | NEW-M2 | Low | 1 line |
| Return affected count from `update_edge_target` | NEW-M1 | Low | Small |
| Add `Error::JsonRpcParse` variant | NEW-M8 | Medium | Small |
| Remove `character_arc` field (perpetual None) | NEW-K4 | Low | Small |
| Update docs: `lore_scope` tool name | H2 | Low | Doc |
| Fix `entity_timeline` chapter 0 sentinel | NEW-K12 | Low | Small |
| Remove debug `eprintln!` in production | NEW-I10 | Low | Small |
| Fix sentence offset trimming | NEW-H26 | Low | Small |

The remaining architectural CRITICAL/HIGH items (dead compiler module, stub layers, empty JSON profiles, WORLD_SCHEMA, transaction wrapping, hardcoded novels) are tracked as tech debt for a dedicated V7 wiring sprint.

---

## Source Reports (v2)

- `/tmp/review_compiler_v2.md` — 33 findings (compiler pipeline)
- `/tmp/review_knowledge_v2.md` — 15 findings (knowledge store + migrator)
- `/tmp/review_mcp_cli_v2.md` — 18 findings (MCP server + CLI)
- `/tmp/review_ingest_v2.md` — 13 findings (ingest V1 pipeline)
