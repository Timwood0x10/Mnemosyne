# Code Review — memory_distill（Cognitive Memory MCP Server）

> 审查范围：全部 `src/**/*.rs`（21 文件）、`Cargo.toml`、`Makefile`、`README*`、`plan/`、`docs/`、仓库工程卫生。
> 审查方式：静态通读 + 实际 `cargo test`（后台运行，142 单测 + 3 doctest 全过）。
> 结论先行：**代码质量高、可编译、测试通过；但有一个「核心功能失效」级 bug（冲突解决阶段实际从未触发），外加若干文档/工程卫生问题，暂不推荐直接开源。**

---

## 1. 项目完成度

**整体：可用 MVP（约 75%），但「8 阶段流水线」名不副实。**

| 能力 | 状态 | 说明 |
|------|------|------|
| 6 个 MCP 工具（distill/compile/search/store/feedback/stats） | ✅ 已实现 | 工具注册、JSON-RPC 循环、stdio 传输均可用 |
| 提取 / 分类 / 评分 / 过滤 / 压缩 / 嵌入 / 持久化 | ✅ 已实现 | 管线主体完整 |
| 对话编译（goal/module/files/problems/reasoning_chain） | ✅ 已实现 | `compiler.rs` 逻辑完整 |
| 检索（keyword / vector / hybrid） | ⚠️ 部分 | keyword 可用；vector/hybrid 需外部 embedding，且见 Bug #1 |
| **冲突解决（ReplaceOld/KeepBoth）** | ❌ **失效** | 见 Bug #1，默认及开启 embedding 下都不触发 |
| 容量控制（LRU 淘汰） | ✅ 已实现 | `phase_enforce_capacity` 正常 |
| memory_feedback（Evolution） | ⚠️ 桩 | 仅 `tracing::info!` 记录，无持久化（README 已注明 "for future"） |
| SSE 传输 | ❌ 未接线 | `--sse-addr` 配置存在但 `main` 永远走 stdio（见 Bug #6） |
| 测试 | ✅ 142 单测 + 3 doctest 全过 | 覆盖到各模块单元，但**未覆盖集成后的冲突解决** |

**证据**：`cargo test` 输出 `test result: ok. 142 passed; 0 failed` + `Doc-tests: 3 passed`。与 README 声称的「141+ 单测 / 3 文档测试」一致。

---

## 2. 能否开源？——暂不建议，需先处理 4 件事

| 检查项 | 结果 |
|--------|------|
| LICENSE | ✅ `Cargo.toml` 声明 `Apache-2.0`，根目录 `LICENSE` 文件存在 |
| .gitignore / 敏感文件 | ✅ 忽略 `/target`、`*.db`、`plan/`、`.codescope/`；`memory.db`、大二进制**未被跟踪**；`.cargo/config.toml` 仅设并行编译，**无密钥泄露** |
| 上游派生合规 | ⚠️ 设计文档与代码表明派生自 `github.com/Timwood0x10/ares`（Apache-2.0）。**需补充 NOTICE / 原作者署名**，确认 Apache-2.0 衍生要求 |
| **文档主题一致性** | ❌ **严重**：`docs/` 共 61 个文件，几乎全是「goagentx / Agent Harmony Protocol」这个**庞杂上游项目**的深度文档（工作流引擎、飞行记录仪、知识图谱、量化交易…），与本 memory-distill 项目无关。发布前必须清理/重写 |
| 贡献/发布元数据 | ❌ 缺 `CONTRIBUTING.md` / `CHANGELOG.md` / `SECURITY.md` / `.github/`（CI、PR 模板）、`examples/`（目录为空）、`Cargo.toml` 未声明 `rust-version`（edition=2024 需 rustc≥1.88） |
| 文档与实现同步 | ❌ 见 Bug #4/#6/#7，README 与代码多处不一致 |

**建议开源前的清单**：
1. 清理 `docs/`（保留/改写真正与本仓库相关的部分，如 `memory-distillation-deep-dive`），删除 goagentx 文档；
2. 修复 Bug #1（冲突解决失效）；
3. 补全 README / `lib.rs` / `main.rs` 注释中与实现不符的工具数量、名称、`mcp` 模块（见 Bug #7）；
4. 增补 `CONTRIBUTING` / `CHANGELOG` / `SECURITY` / `.github` CI / `rust-version`。

---

## 3. 潜在 Bug（按严重度）

### 🔴 #1 冲突解决阶段实际从未触发（核心功能失效）
**位置**：`src/distiller.rs:342-400`（`phase_resolve_conflicts`），`src/store.rs:185-208`（`row_to_experience`），`src/store.rs:321-326`（`search_by_vector`）。

**原因链**：
- `row_to_experience` 在第 201 行硬编码 `vector: Vec::new()`，即 `SELECT * FROM memories` 取出的 `Experience` **向量永远为空**；`search_by_vector` 的 SQL 只 `SELECT m.*, distance`，也未把 `vec_memories.vector` 取回。
- 因此 `phase_resolve_conflicts` 里对每个候选 `exp` 构造的 `existing_mem = Memory::new(...)` 向量也为空；
- `resolver.resolve(&mem, &existing_mem, ...)` 计算 `cosine_similarity(&mem.vector, &existing_mem.vector)` → 一侧为空 → 永远返回 `None` → `Resolution::NoConflict`。

**后果**：
- keyword 模式（默认，`NullEmbedder`）：`phase_embed` 是 no-op，`mem.vector` 为空，第 346 行直接 `kept.push(mem); continue;` → 整段跳过；
- 开启 embedding 的 vector/hybrid 模式：`search_by_vector` 能返回候选，但 `resolver` 拿到的 existing 向量永远为空 → **仍永远 `NoConflict`**。

也就是说，**无论是否启用 embedding，冲突解决都不会执行替换（ReplaceOld）或保留（KeepBoth）**；`memories_replaced` / `conflicts_resolved` 指标在真实冲突下永不为正，重复记忆会被反复写入。README/架构图把冲突解决列为 8 阶段之一，但实际是死代码。
> 附带：第 356 行 `let existing = self.store.get_by_memory_type(...)` 紧接着 `let _ = existing;` 是**无效 DB 查询 + 死代码**，暴露作者本想取带向量的已有记忆却未接上。

**修复方向**：让 `search_by_vector` 同时返回 `vec_memories.vector` 并在 `row_to_experience` 中回填 `exp.vector`；删掉第 356 行无效查询；把 `exp.vector`（真实向量）传给 `resolver.resolve`。

---

### 🟠 #2 FTS5 搜索把用户原始 query 直接拼接进 `MATCH`，特殊字符会抛语法错误
**位置**：`src/store.rs:353` 与 `:361`：`... WHERE memories_fts MATCH ?1 ...`。

`query` 是绑定参数，但 FTS5 仍会把它当**查询表达式**解析。用户输入含 `"`、`(`、`*`、`:`、`-` 等字符时会触发 `fts5: syntax error`，整个 `search_by_keyword` 抛错，进而 `memory_search` 工具失败。这是默认 keyword 模式的稳健性 bug。
**修复**：把 query 转义为短语——内部 `"` 双写后整体用双引号包裹，或先做 `escape` 再 `MATCH '"..."'`。

---

### 🟠 #3 编译器对编译出的 knowledge 硬编码租户 `"default"`
**位置**：`src/compiler.rs:189`：`let mut mem = Memory::new("default", memory_type, &raw.solution, importance);`

`memory_compile` 返回的 `knowledge` 数组一律使用 `"default"` 租户，忽略了调用方传入的 `tenant_id`。虽然这部分 knowledge 当前**不被持久化**（只有 distill 路径与 decisions 被落库），但输出字段的租户是错的，且一旦将来把 compile 的 knowledge 也落库，租户隔离就会被破坏。

---

### 🟡 #4 `memory_compile` 的 `distill` 默认值：schema 说 false、代码默认 true、README 说 true
**位置**：`src/main.rs:491`（`"default": false`）vs `src/main.rs:267`（`unwrap_or(true)`）；README 表写「默认 true」。

行为实际是 **true**（handler 的 `unwrap_or(true)`），但对外 schema 声明 false。依赖 schema 的 MCP 客户端会误以为默认不蒸馏，产生意外开销。统一为 `true` 或 `false` 其一即可。

---

### 🟡 #5 分类器只会产出 4 种 memory_type，Skill/Experience 永远靠手动写入
**位置**：`src/classifier.rs:15-87`（`TYPE_KEYWORDS` 仅含 Profile/Preference/Interaction/Knowledge）。

文档/README 列出 6 种记忆类型（含 `skill`、`experience`），但分类器没有这两类的任何关键词 → **蒸馏管线永远不会产生 skill / experience 记忆**。这两类只能经 `memory_store` 手动写。若这是设计取舍应注明，否则属于与文档不符的能力缺口。

---

### 🟡 #6 SSE 配置存在但未接线，永远走 stdio
**位置**：`src/main.rs:517-523`（只 `match Serve | None` → `StdioTransport`）；`Config.sse_addr` 在 `build_server` 中未被读取。

`--sse-addr` / `MEMORY_SSE_ADDR` 进了 `Config` 也经 `validate`，但服务器永远用 stdio。属于「配置支持但功能未实现」的未完成项。

---

### 🟢 #7 文档与实现不一致（多处）
- `src/lib.rs:8-9` 写着「5 个工具（distill/search/list/delete/stats）」——实际 6 个且名称不同（历史残留）。
- `src/main.rs:4-6` 注释称注册「5 个 `memory_*` 工具」并列举，**漏了 `memory_compile`**。
- `README*` 的「Project structure」未列出实际存在的 `src/mcp/`（transport/types/server）子模块。
- `README.md` 与 `README.zh.md` 写 `MEMORY_VECTOR_DIM` 默认 `1024`，但 `Config::default().vector_dim = 0`（且 CLI 路径在 provider=none 时自动归零）。
- `Makefile` 的 `make test` 依赖 `cargo nextest`，未装 nextest 的协作者会构建失败（建议回退到 `cargo test` 或注明依赖）。

---

### 🟢 #8 其他小问题（低优先级）
- **`tools/call` 不校验 `arguments` 是否符合 `input_schema`**：`src/mcp/server.rs:217-271` 注释声称「light validate」，实际只交给各 handler 自行 `ok_or_else` 解析；schema 仅作文档。
- **stdio `recv` 阻塞运行时线程**：`src/mcp/transport.rs:52-71` 用同步 `read_line` 未 `spawn_blocking`，单 worker 长期阻塞时可能影响并发（stdio 场景通常不致命，但属反模式）。
- **`Config::from_env()` 不调用 `validate()` 且不归零 `vector_dim`**：`src/config.rs:197-260`；与 `into_config()`（CLI 路径）行为不一致。当前二进制只用 CLI 路径，该 env 路径属死代码。
- **`phase_embed` 任一 embedding 失败即中断整个 distill**：`src/distiller.rs:320-335` 把 `_` 错误包成 `Err` 返回，缺「跳过 embedding 退回 keyword」的优雅降级。
- **`tenant_locks` HashMap 无驱逐**：`src/distiller.rs:172`，长周期多租户服务会缓慢泄漏内存（极低优先级）。
- **`#![allow(dead_code)]` on `MemoryFeedbackTool`**（`src/main.rs:127`）：该工具已被注册，allow 多余；`NoiseFilter::retain_indices` 为 `pub` 但疑似未被使用。

---

## 4. 验证小结

- ✅ 编译通过（edition 2024，rustc 1.95），无测试失败。
- ✅ 单元测试覆盖到提取/分类/评分/过滤/压缩/检索/存储/冲突解决器（孤立测试）/MCP 协议等；但**冲突解决器的集成路径（#1）未被测试覆盖**，导致该失效未被发现。
- ⚠️ 建议补充：① 冲突解决「插入重复向量后应 ReplaceOld」的集成测试；② FTS5 含特殊字符 query 的搜索测试；③ 跑 `cargo clippy`（Makefile `make check`）确认无 lint 警告。

---
*本评审为只读分析，未修改任何项目源码。*
