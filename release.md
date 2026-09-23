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
- **Evidence anchors are now persisted.** Compiled facts used to keep the
  original utterance only inside their payload while `evidence_id` stayed NULL,
  which left the `evidence` table empty in production: `fact_provenance`'s
  "why do we believe this?" answered `null` for every real fact and
  `state_timeline` intervals never carried an `evidence_ids` entry. The write
  path now registers the anchor (one row per utterance, shared by the facts it
  produced).
- **Optional `tenant_id`** on `state_timeline`, `fact_provenance`,
  `decision_trace` and `decision_search`; a cross-tenant id is reported as
  not-found instead of being served.

### Fixed

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

## Docs

- [README](README.md) / [README.zh.md](README.zh.md)
- [Architecture](docs/en/architecture.md) / [Architecture (zh)](docs/zh/architecture.md)
- [Module docs](docs/en/compiler.md) / [Module docs (zh)](docs/zh/compiler.md)

## License

Apache-2.0
