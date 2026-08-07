# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed

- **V7 接线 sprint：清掉编译器死代码。** `src/compiler/` 下七个带 TODO 的模块
  （`alias.rs` / `pronoun.rs` / `merge.rs` / `relation.rs` / `inference.rs` /
  `entity/conversation.rs` / `entity/regex.rs`）从未在 `mod.rs` 声明且零引用，
  已删除；`entity/mod.rs` 中对应的 `ConversationProvider` / `RegexProvider`
  导出一并移除。
- **NovelProvider 接入生产编译链路。** `compile_source`（`generalize_compile`
  工具）现在会从小说角色字典注册"文本中实际出现（名称或别名）"的已知角色，
  解决评审 NEW-C20（profile JSON `entities` 全空 + NovelProvider 从未实例化）；
  角色带 `source=novel_dictionary` 标记与别名属性入库，并同步 V7 world 实体。
- 新增端到端验证：`tests/mcp_corpus_full_loop.rs::generalize_then_inspect_entity_e2e`
  走真实 MCP `tools/call` 路径（generalize_compile → inspect_entity），确认
  V7 链路 compile → graph → 查询成立。
- **迁移事务包裹（评审 H6）。** `Migrator::migrate` 现在把整轮 V1→general
  迁移包在一个 SQLite 事务里（`SQLiteKnowledgeStore::begin/commit/rollback_transaction`），
  中途失败即整体回滚，不再留下半迁移数据库（有文档、缺章节；边悬空等）。
  新增三个 store 级事务测试（commit 持久 / rollback 丢弃 / 多行回滚）。
- **全量语料验收回归。** 新增 `tests/generalize_corpus_regression.rs`：对 7 部
  小说语料（三国演义/水浒传/红楼梦/西游记/封神演义/倾城之恋/PrideAndPrejudice）
  + 3 组对话语料跑 `compile_source` → `inspect_entity` 全链路，全绿
  （三国演义 3644 objects / 55763 edges，红楼梦/封神演义检出故事事件）。

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
