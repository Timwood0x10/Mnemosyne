# Mnemosyne v0.1.2

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
- **33+ MCP tools** — `memory_compile`, `generalize_compile`,
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

## Changelog (v0.1.2)

### Fixed

- **Prebuilt binaries no longer hardcode the build machine's source path.**
  `env!("CARGO_MANIFEST_DIR")` was baked into every release, so a binary
  built on CI panicked with `Failed to load core lexicon from
  config/dictionary.json` (`FileLoad { path: "/Users/runner/work/..." })` on
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

## Docs

- [README](README.md) / [README.zh.md](README.zh.md)
- [Architecture](docs/en/architecture.md) / [Architecture (zh)](docs/zh/architecture.md)
- [Module docs](docs/en/compiler.md) / [Module docs (zh)](docs/zh/compiler.md)

## License

Apache-2.0
