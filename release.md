# Mnemosyne v0.1.3

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
required), and ~6–8 MB after release optimization. **They no longer embed a
compile-time source path** — resources are resolved at runtime, so a released
binary runs on any machine, not just the CI builder that produced it.

Each release also ships the two customizable observation-marker word lists —
`markers_zh.json` (Chinese) and `markers_en.json` (English). Put them in the
same directory as the binary to enable customizing which words produce facts;
edit them freely and re-run without recompiling. When absent, the binary
falls back to built-in defaults and keeps working.

## Quick start

```bash
# 1. Local stdio mode (IDE / Claude Desktop / Cursor integration)
./mnemosyne serve

# 2. Remote HTTP+SSE mode (token required; session isolation per client;
#    default listen port is 5609)
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
- **Rich MCP tool set** — `memory_compile`, `generalize_compile`,
  `persona_check`, `persona_inject`, `cognitive_context`, `inspect_entity`,
  `timeline`, `relation_graph`, `search_graph`, `trace_path`, `memory_decay`,
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

## Changelog (v0.1.3)

### Added

- **Cognitive state history** — `state_timeline` reconstructs an entity's
  per-dimension state intervals (validity window + evidence anchors) and the
  deterministic transitions between them.
- **Fact provenance** — `fact_provenance` audits confidence, the three-state
  epistemic status, the original-text evidence and the `derived_from` chain.
- **Decision write path** — `memory_compile` compiles explicit commitments into
  `Decision` records anchored to an Event fact for the utterance;
  `decision_trace` and `decision_search` read them back.
- **Decision closure** — the same call accepts a `decision_outcomes` array so the
  host declares what happened to an earlier commitment (nothing is inferred; the
  first outcome recorded wins). Previously every decision stayed `open` forever.
- **Evidence anchors are now persisted.** Compiled facts used to keep the
  original utterance only inside their payload while `evidence_id` stayed NULL,
  which left the `evidence` table empty in production: `fact_provenance`'s
  "why do we believe this?" answered `null` for every real fact and
  `state_timeline` intervals never carried an `evidence_ids` entry. The write
  path now registers the anchor (one row per utterance, shared by the facts it
  produced).
- **Self-disclosure channel** — `compile_user_facts` now runs two complementary
  extractors: the observation marker table (feelings / wants / dislikes) and
  `self_disclosure.rs`, which compiles name / age / occupation / city / family /
  pets / interests / habits. Those types were unreachable before (measured recall
  0%, now 100%).
- **Compile-yield measurement** — `tests/compile_yield.rs` runs an annotated
  colloquial corpus through the real compiler and reports recall / phantom /
  over-extraction (baseline: explicit signals 100%, phantoms 0/9, capability gaps
  22%), documented in `docs/zh/compile-quality.md`.
- **Optional `tenant_id`** on `state_timeline`, `fact_provenance`,
  `decision_trace` and `decision_search`; a cross-tenant id is reported as
  not-found instead of being served.

### Fixed

- **The user-fact pipeline existed in two places**, and the new self-disclosure
  channel was wired into one of them only: `memory_compile` inlined the marker
  path, so a self-introduction produced no identity facts over MCP while the
  corpus test reported 100%. Both callers now share one entry point, covered by
  the corpus harness *and* an MCP end-to-end test.
- **A single negation word anywhere in a sentence discarded an affirmative plan**
  ("想学吉他很久了，一直在纠结买不买" lost its goal to the `买不买` cue): negation is
  now resolved per marker inside its own clause, which took explicit-signal recall
  from 84% to 100% on the new corpus.
- **A negated statement was reported as the current state** — `aggregate` listed
  "我不喜欢应酬" as a *preference*; negated facts are now excluded from the
  affirmative current-state projection and kept in the history layer instead.
- **Negated goals were dropped entirely**, so "我不打算考公务员了" was lost; they are
  now kept as negated facts, which still never surface as an active goal.
- **Cognitive-state transitions were unreachable on production data.** A
  `gradual_change` required a `keyword` payload field while the comparison text
  came from `content`, and no compiler emits both — so the plan's own example
  ("喜欢独处 → 开始想社交 → 喜欢热闹") could never be reported. The comparison
  text now falls back through `content` → `object` → `keyword` → `action`.
- **A user's negated statements were discarded at compile time**, so the engine
  could not represent a negative stance and `stance_flip` was structurally
  impossible for a user entity. Negated observations now keep their fact with
  `negated: true` (only `Goal` is still suppressed); the affirmative side
  declares `negated: false` so both sides of a flip are readable.
- **`state_timeline` returned zero dimensions for every real conversation**, and
  every transition was pinned to the last two intervals once a dimension had
  three or more states.
- **One state could be reported as an interval per observation** when a payload
  carried an ever-growing counter (`occurrences`) in its fold key.
- **`memory_decay` down-weighted almost the whole store on its first pass** — an
  absent `access_count` was scored as "never accessed". Unknown access history is
  now neutral; only an explicit `0` decays.
- **A legacy database with a NULL `weight` could not be opened at all**; the
  `confidence` migration now uses `COALESCE(weight, 1.0)`.
- **Commitment extraction could record the opposite of what was said**
  ("compromise" matched the `promise` marker; "I will not help you" became a
  commitment). Markers now need a word boundary and are cancelled by a negation
  cue.
- **A rejected decision left half a compilation behind**; facts, anchors and
  decisions are now validated first and committed in ONE transaction.
- **`set_decision_outcome` could overwrite a recorded outcome**; the guard now
  lives in the SQL statement itself.
- **`decision_search` did not escape LIKE wildcards and did not clamp `limit`.**
- **`insert_decision` never validated its input.**
- **`state_timeline` published a fabricated evidence link** (`fact_ids: [0]` for
  a fact that has no id).
- **Stale documentation** — the `confidence` field still described itself as
  derived from the decay `weight`, two `plan/…` links were dangling, and the
  tool's dimension list was a hand-maintained copy of the engine's table.

### Changed

- **Large modules split to satisfy the one-file-per-1000-lines rule**
  (`fact_store`, `cognition`, `store`, `retrieval`, `conversation_compiler`,
  `character`, `distiller`, `lexicon`, `knowledge/store`,
  `compiler/name_validation`, plus the binary tool handlers).
- **All `#[allow(...)]` suppressions removed** and library warnings moved to
  `tracing`; the transition tests live in `tests/state_transitions.rs`.
- **Dead thin wrappers dropped** (`RelationshipStore::upsert_relationship`,
  `SqliteFactStore::save_relationship`, `decay::archive_fact`,
  `decay::list_archived`): every one of them only forwarded to a method the
  production paths already call.
- **Logging defaults to `warn`** when `RUST_LOG` is unset or unparsable, so the
  library's degradation warnings (missing config, embedding failures) are visible
  without configuring logging first. `RUST_LOG` still overrides it, and stdout
  stays clean for JSON-RPC.
- **Print-only helpers dropped** (`ResolverStats::print_report`,
  `FactionReport::print_report`) and one test that asserted nothing replaced by a
  real assertion.
- **The release job refuses a version mismatch.** The tag is derived from this
  file's first line, so a `Cargo.toml` that disagreed would have published a tag
  the binary does not claim; the workflow now fails before creating the release.

## Docs

- [README](README.md) / [README.zh.md](README.zh.md)
- [Compile quality baseline](docs/zh/compile-quality.md) / [编译产出质量基线](docs/zh/compile-quality.md)
- [Architecture](docs/en/architecture.md) / [Architecture (zh)](docs/zh/architecture.md)
- [Module docs](docs/en/compiler.md) / [Module docs (zh)](docs/zh/compiler.md)

## License

Apache-2.0
