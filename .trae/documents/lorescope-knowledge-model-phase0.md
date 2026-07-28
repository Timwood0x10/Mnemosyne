# LoreScope Phase 0 — General Knowledge Model (Object + Edge + Evidence)

## Context

`docs/zh/dev_guide.md` (V2.0 冻结版) redefines LoreScope as a **Narrative World
Compiler**: a non-LLM knowledge distillation engine that compiles narrative text
into a queryable, evidence-backed world model. Phase 0's acceptance test is:

> `inspect_entity("赵云")` 跑通，返回 Object + Edges + Evidence.

Today the project only has the **V1 domain model** (`character_attributes` /
`character_events` / `character_relations`, in [src/character.rs](file:///Users/scc/code/rustcode/memory_distill/src/character.rs))
plus 10 MCP tools in [src/main.rs](file:///Users/scc/code/rustcode/memory_distill/src/main.rs).
The frozen general model (`knowledge_objects` / `knowledge_edges` / `evidence` /
`mentions` / `documents` / `chapters` / `knowledge_evidence` / `compiler_runs`)
does not exist yet, and `make check` is currently red because `examples/` and
`tests/` still import the old crate name `memory_distill` (the lib is now `lore_scope`).

Per the migration strategy (dev_guide §6): **no double-write**. V1 stays as a
legacy read view; new data goes only into the general tables. So this work
*adds* the general model alongside V1 — V1 functionality is untouched.

**Outcome:** a new `knowledge` + `storage` module pair, a one-time V1→general
migrator, and 4 new MCP tools (`inspect_entity` / `timeline` / `relation_graph`
/ `evidence`), with `make check && make test && make fmt` green (0 error, 0 warning).

---

## Approach

### Step 0 — Unbreak the baseline (multi-agent, parallel, independent)
Fix the stale crate import in `examples/*.rs` and `tests/*.rs`:
`memory_distill::` → `lore_scope::` in 5 files, and update
`examples/claude-desktop-config.json` `RUST_LOG: memory_distill=info` → `lore_scope=info`.
Then `cargo check` is green before any new code lands.

### Step 1 — `Cargo.toml`
Add `petgraph = "0.6"` (frozen decision §6: "SQLite + petgraph"). Used by
`relation_graph` for in-memory DiGraph BFS; also a foundation for the roadmap's
community-discovery / subgraph-export.

### Step 2 — `src/storage/` (schema SQL, frozen)
- `src/storage/mod.rs` — re-exports.
- `src/storage/schema.rs` — `pub const KNOWLEDGE_SCHEMA: &str` containing the 8
  frozen tables + indexes from dev_guide §3, verbatim DDL:
  `documents, chapters, knowledge_objects, knowledge_edges, knowledge_evidence,
  evidence, mentions, compiler_runs`. `sentences` table is OPTIONAL → skipped.

### Step 3 — `src/knowledge/mod.rs` (structs + enums)
Frozen-schema structs, ids are `i64` (INTEGER PK AUTOINCREMENT), `created_at`
are `i64` unix seconds (matches `strftime('%s','localtime')`), `properties` is
`serde_json::Value` (matches `JSON DEFAULT '{}'`):
- enums: `ObjectType` (Person/Event/Place/Organization/Concept/Artifact/Role),
  `Origin` (Observed/Derived), `EvidenceSourceType` (Object/Edge) — each with
  `as_str()` + `FromStr` (drives the SQL `CHECK` columns).
- structs: `Document`, `Chapter`, `KnowledgeObject`, `KnowledgeEdge`,
  `Evidence`, `KnowledgeEvidenceLink`, `Mention`, `CompilerRun`.
- result DTOs for tools: `InspectEntityResult`, `TimelineEntry`, `GraphNode`,
  `GraphEdge`, `RelationGraphResult`, `EvidenceHit`.

### Step 4 — `src/knowledge/store.rs` (trait + SQLite impl)
`KnowledgeStore` trait + `SQLiteKnowledgeStore` (mirrors `SQLiteCharacterStore`
in [src/character.rs:289](file:///Users/scc/code/rustcode/memory_distill/src/character.rs)):
- `open(path)` / `open_in_memory()` / `init()` runs `KNOWLEDGE_SCHEMA`.
- CRUD: documents, chapters, objects, edges, evidence, links, mentions, runs.
- Reuse patterns from [src/character.rs](file:///Users/scc/code/rustcode/memory_distill/src/character.rs)
  (`Arc<Mutex<Connection>>`, `params!`, `Box<dyn ToSql>` for optional-novel filters).
- High-level queries backing the 4 MCP tools:
  - `inspect_entity(name, doc?) -> InspectEntityResult` (object + person↔person
    edges + participated_in event edges + evidences + mentions)
  - `entity_timeline(name, doc?) -> Vec<TimelineEntry>` (ordered by chapter_no)
  - `relation_graph(name, depth) -> RelationGraphResult` (petgraph BFS, depth
    default 2 / max 5; surfaces `valid_from`/`valid_to`)
  - `search_evidence(query, doc?, limit) -> Vec<EvidenceHit>`

### Step 5 — `src/knowledge/migration.rs` (one-time V1 → general)
`Migrator { v1: &SQLiteCharacterStore, knowledge: &SQLiteKnowledgeStore, corpus_dir }`
→ `migrate() -> MigrationStats`. Inputs: V1 `character_*` tables (objects/edges
metadata) + corpus text (documents/chapters/evidence content + offsets). Steps:
1. For each novel: `corpus::load_novel` → `Document` + `Chapter` rows with byte
   offsets (cumulative). Maps: `novel→doc_id`, `(novel,ch_no)→chapter_id`.
2. V1 `character_attributes` → `KnowledgeObject(person)`, properties =
   `{aliases, clothing, personality, description, novel, importance, tenant_id}`.
   Map `(novel,name)→object_id`.
3. V1 `character_events` → `KnowledgeObject(event)` + `KnowledgeEdge(person→event,
   "participated_in", origin=observed, valid_from=chapter)`.
4. V1 `character_relations` → `KnowledgeEdge(person→person, predicate=relation_type,
   origin=observed, valid_from=chapter, valid_to=NULL)`, properties carry the V1
   dimension scores (`co_occurrence_score` etc. from V1 `metadata`) — preserves
   the 3D scoring work with **no functional discount**.
5. Evidence + mentions: per person, scan each chapter of its novel for canonical
   name + aliases via `str::match_indices` (char-boundary-safe, reusing
   [`extract::floor_char_boundary`](file:///Users/scc/code/rustcode/memory_distill/src/ingest/extract.rs));
   emit **≤1 mention+evidence per (character, chapter)** to bound size; each
   event/relation also gets 1 evidence (content = its V1 description, chapter_id =
   its chapter) linked via `knowledge_evidence`.

All 4 novels are migrated (consistent with ingest); the 三国 acceptance test
verifies `inspect_entity("赵云")`.

### Step 6 — `src/mcp/knowledge_tools.rs` (4 tool handlers + registrar)
Define `InspectEntityTool`, `TimelineTool`, `RelationGraphTool`, `EvidenceTool`
(each holds `Arc<SQLiteKnowledgeStore>`, implements `ToolHandler` per
[src/mcp/types.rs](file:///Users/scc/code/rustcode/memory_distill/src/mcp/types.rs))
and a `register_knowledge_tools(builder, store)` helper. Putting these here keeps
`main.rs` under the 1000-line rule (it is already 927 lines).

### Step 7 — Wire into `config.rs` + `main.rs`
- `config.rs`: add `Command::Migrate { corpus_dir }` to the existing
  `Command` enum ([src/config.rs:378](file:///Users/scc/code/rustcode/memory_distill/src/config.rs)).
- `main.rs`: in `build_server` construct `SQLiteKnowledgeStore::open(&cfg.db_path)`
  and call `register_knowledge_tools(&mut builder, kstore)`; add a `Migrate` arm
  that opens both stores and runs `Migrator::migrate`.

### Step 8 — Tests (module-by-module, per rules.md)
- `storage/schema.rs`: idempotent init; all 8 tables exist.
- `knowledge/store.rs`: CRUD round-trips; `inspect_entity` returns
  Object+Edges+Evidence; timeline ordering; relation_graph depth/temporal;
  duplicate-evidence `UNIQUE` constraint; missing-entity → empty/Err.
- `knowledge/migration.rs`: synthetic mini V1 dataset + tiny corpus → assert
  counts & post-migration `inspect_entity`.
- `tests/knowledge_migration.rs` (integration): ingest 三国演义 → migrate →
  `inspect_entity("赵云")` non-empty. (Mirrors existing `tests/ingest_integration.rs`.)
- All asserts carry explicit messages (rules §II.2); no `println!` in tests.

### Step 9 — Verify & review (multi-agent)
- `cargo fmt --all` → `make check` (0 error, 0 warning — fix root causes, **no**
  `#[allow(..)]` per rule 5) → `make test` (or `cargo nextest`).
- `cargo run --example zhuge_network` confirms V1 still works post-import-fix.
- Spawn the **TRAE-code-review** skill on the final diff.

---

## Critical files

| File | Change |
|------|--------|
| [Cargo.toml](file:///Users/scc/code/rustcode/memory_distill/Cargo.toml) | + `petgraph = "0.6"` |
| [src/lib.rs](file:///Users/scc/code/rustcode/memory_distill/src/lib.rs) | + `pub mod knowledge; pub mod storage;` |
| `src/storage/mod.rs`, `src/storage/schema.rs` | NEW — schema DDL |
| `src/knowledge/mod.rs` | NEW — structs + enums |
| `src/knowledge/store.rs` | NEW — `KnowledgeStore` + SQLite impl |
| `src/knowledge/migration.rs` | NEW — V1 → general migrator |
| `src/mcp/knowledge_tools.rs` | NEW — 4 tool handlers + registrar |
| [src/config.rs](file:///Users/scc/code/rustcode/memory_distill/src/config.rs) | + `Command::Migrate` |
| [src/main.rs](file:///Users/scc/code/rustcode/memory_distill/src/main.rs) | build+register knowledge store; `Migrate` arm |
| `examples/{zhuge_network,lubo_network,lubo_verified,diag_relation}.rs`, `tests/{chap_diag,ingest_integration}.rs` | `memory_distill::`→`lore_scope::` |
| `examples/claude-desktop-config.json` | `RUST_LOG` value |
| `tests/knowledge_migration.rs` | NEW integration test |

## Verification
1. `make fmt` && `make check` → 0 error / 0 warning.
2. `make test` → all green (incl. new knowledge tests).
3. `cargo run -- migrate --corpus-dir corpus` then `inspect_entity("赵云")`
   via MCP returns Object + Edges + Evidence.
4. `cargo run --example zhuge_network` → V1 path still works (no regression).
5. TRAE-code-review skill run on the diff.
