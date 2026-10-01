# 评审状态总表（唯一权威状态文档）— 2026-09-30

> 本文件是仓库内**唯一**的评审状态来源。历史上的四份评审正文（`review-2026-09-25.md`、`review-2026-09-26.md`、
> `review-2026-09-27.md`、`review-fixes-2026-09-27.md`）编号彼此冲突，且前三份已在**收敛时并入本文件**并随之移除
> （保留的第四份 `review-fixes-2026-09-27.md` 仍在仓库中作为过程记录）。因此历史条目的**正文不再单独存在**，其
> 结论一律以本文件的 CLOSED / OPEN 两张表为准；任何 ID 引用一律按「**报告日期/ID**」消歧，例如 `09-26/H7` 与
> `09-27/H1` 是两条不同的缺陷。
>
> 治理规则以 **`plan/rules/rules.md`** 为准（仓库内**不存在** `code_rules.md`）。

---

## 1. 门禁快照（2026-09-30）

| 检查 | 结果 |
|---|---|
| `make check`（`cargo clippy --all-targets --all-features` + check） | ✅ **0 error / 0 warning** |
| `make test`（cargo nextest） | ✅ **977 passed / 0 failed / 12 skipped** |
| `cargo fmt --all --check` | ✅ clean |
| `cargo clippy --all-targets -- -D warnings`（release.yml lint 门禁） | ✅ clean |
| `cargo test --doc` | ✅ 4 passed / 0 failed（6 ignored） |
| 最长源文件 | 980 行（`src/mcp/memory_compile.rs`）→ 规则 §1（≤1000）通过 |
| `#[allow(...)]` 属性 | 0 处（仅注释中提及该属性） |
| 基线 / 变更 | `dev @ e05442e` 基础上叠加**未提交**的 A–F、G 批次与「后续」批次 |
| 依据规则 | `plan/rules/rules.md`（无 `code_rules.md`） |

---

## 2. 已关闭清单（CLOSED）

按报告分组，ID 均已消歧。

### 2.1 `09-25`（第三轮 · 配置 / 词表加载层）

| ID | 内容 | 说明 |
|---|---|---|
| 09-25/C1–C11 | 桶名白名单、内置兜底词表收敛、`*.user.json` 叠加 + `_remove`、`config-check` 自检、`MNEMOSYNE_HOME` 文档化、`trace_path_tool` 去 `expect`、`too_many_arguments` 参数对象化、贴线文件目录化拆分等 | 全部落地（见 09-25 §8） |

### 2.2 `09-26`（第四轮 · 全库缺陷扫描）

| ID | 内容 | 说明 |
|---|---|---|
| 09-26/C1–C8 | 8 条 Critical 全部修复 | 含单机语义收口、`knowledge_attach` 沙箱加固、中文检索零结果、冲突替换单事务、HTTP 会话签发与 TTL 回收等 |
| 09-26/H1 | `knowledge_attach` 路径穿越 | 已修（canonicalize + 拒绝 config/DB/密钥 + NUL 探测） |
| 09-26/H3 | `?` 提前返回 → `PRAGMA foreign_keys` 永久 OFF | 已修（`with_foreign_keys_disabled` 统一恢复） |
| 09-26/H4 | 重复编译每次新增 evidence 行 | 已修（`ux_evidence_identity` + 幂等 upsert） |
| 09-26/H5 | `lexicon/matcher.rs` AhoCorasick 默认 Standard | 已修（`MatchKind::LeftmostLongest`） |
| 09-26/H6 | `documents(title,source)` 无唯一索引 | 已修（`ux_documents_identity` + `RETURNING id`） |
| 09-26/H8 | session map 永不回收 → 内存耗尽 | 已修（`SessionEntry.last_seen` + `SESSION_TTL` 惰性回收） |
| 09-26/H9 | 容量淘汰平分时删**最新**的，可能删掉本轮新建 | 已修（`(confidence asc, created_at asc)` + `protected` 排除本轮，见 §2.5） |
| 09-26/H10 | 读路径完全不过滤 `expires_at` | 已修（`get`/`search_by_vector`/`search_by_keyword` 两分支/`get_by_memory_type` 均加过期门，边界 `<=`） |
| 09-26/H11 | hybrid 只从 keyword 候选集出结果，向量独有命中被丢 | 已修（候选集 = `keyword ∪ semantic`，向量独有取最差 keyword rank） |
| 09-26/H12 | FTS 索引从不回填；LIKE 兜底漏 `solution` | 已修（幂等回填 + `solution LIKE ... ESCAPE '\'`） |
| 09-26/H13 | 噪声门丢弃 `is_problem` 刚放行的真实问题 | 已修（`is_noise` 前置 `is_problem`） |
| 09-26/H14 | 密钥过滤漏 `password:`（无空格）等写法 | 已修（裸标记 + 赋值分隔符 + 带词边界前缀） |
| 09-26/H15 | story_bridge 部分写入失败后重跑被永久跳过，stats 还报成功 | 已修（按 event id 幂等补写 + 单事务 + stats 回读 store） |
| 09-26/H16 | JSON 解析失败静默换内置规则，零日志 | 已修（`warn!` + 返回 `Result` 暴露错误；`config-check` 真正 parse） |
| 09-26/H17 | 「三层词表 + disable」运行时完全未接线 | 已修（core → packs → user 分层真正生效；`Disabled` 从注册表与 matcher 剔除） |
| 09-26/H18 | 两个全局词典单例失败时降级为**空表** | 已修（`try_init() -> Result` + 启动期 `verify_vocabulary()` fail-loud） |
| 09-26/H19 | 随包 JSON 的 `match` 键被 serde 忽略 | 已修（`#[serde(rename = "match")]` + 真随包 JSON 测试） |
| 09-26/H20 | `negated_before` 只看首次出现，与 `contains` 语义不一致 | 已修（逐次出现，任一次未否定即命中） |
| 09-26/H21 | 4 类事实的 evidence span 恒为 1 字节 | 已修（`Anchor::value` / `Anchor::fragment` 承载真实长度） |
| 09-26/L9 | 空 token 被接受 + 长度不等的早退泄漏 token 长度 | 已修（空白 token 启动即拒 + 定长比较） |

> **部分关闭**：`09-26/H2`（导出丢 `mentions` / 导入无事务 / 静默跳过仍报成功）——子项（mentions 段、事务、
> `unresolved_references` 计数、span 优先重链接）均已修复，属"大部分完成"。

### 2.3 `09-27`（独立深度评审）

| ID | 内容 | 说明 |
|---|---|---|
| 09-27/H1 | `jaccard` 双空集返回 `1.0` → 任意两单字名"完全匹配" | 已修（双空 → 0.0 + 字节恒等捷径） |
| 09-27/H2 | `AhoCorasick` 默认 `MatchKind::Standard`（生产小说管线） | 已修（`extract.rs` ×3 + `observation_compiler.rs` ×1） |
| 09-27/H3 | HTTP session map 无限增长 DoS | 已修（对应 09-26/H8 的实现） |
| 09-27/M1 | `LexiconMatcher` 的 `Standard` 匹配模式问题 | 已修（对应 09-26/H5） |
| 09-27/M4 | 无 session-id 客户端共享全局 broadcast → 跨客户端泄漏 | 已修（删除全局广播通道） |
| 09-27/M5 | session id 未与鉴权绑定 → 响应劫持 | 已修（服务端签发 + 强制校验） |
| 09-27/M7 | BruteForce 接受 NaN/Inf 而 HNSW 拒绝 → 索引不一致 + 非全序 | 已修（拒绝非有限值 + `id` 升序 tie-break） |
| 09-27/M8 | BruteForce 接受零维（空）向量而 HNSW 拒绝 | 已修（`build`/`search` 在 HNSW 同一边界拒绝） |
| 09-27/M9 | LIKE 只转义 `%`/`_`，未先把 `\` 翻倍 | 已修（`like_pattern`：先 `\` 后 `%`/`_`） |
| 09-27/M10 | 纯 Vector 模式静默丢弃无 embedding 行 | 已收窄（README 明确 Vector 仅对有嵌入行排序；Hybrid 不丢） |
| 09-27/M13 | 否定词表少于文档（`别/未/无/非` 被忽略） | 已修（8 条线索齐全） |
| 09-27/M14 | 整句范围否定过度抑制真实情感 | 已修（紧邻规则；并加 `NON_NEGATION_TAILS` 防 `特别` 等词尾误伤） |
| 09-27/M15 | assistant 消息也改亲密度，与"仅用户 ±0.02"不符 | 已对齐（契约改为用户 ±0.02 / Agent ±0.01，README 同步） |
| 09-27/M16 | 关键词路径 `overlap >= 1` 即判 conflict | 已修（复用 `STANCE_FLIP_MIN_SHARED_BIGRAMS`=2） |

### 2.4 `review-fixes-2026-09-27.md` 的自身编号 R1–R12

| ID | 内容 | 说明 |
|---|---|---|
| 09-27-fix/R1–R12 | 迁移原子修复、索引定义比对、证据租户继承、导入单事务、mentions 往返、span 优先重锚定、容量配额契约、`user_id` 归一化、`replace_batch` 单事务、`profile.rs` 拆分、导入路径去 `expect` 等 | 全部落地（见该文件 §1、§6、§7、§8） |

### 2.5 本会话批次（2026-09-29 独立复核后确认）

| 批次 | 主题 | 条目 | 结论 |
|---|---|---|---|
| A | 生产正确性 | `09-27/H1`、`09-27/H2` | 已修；独立复核确认契约满足 |
| B | 检索与存储 | `09-27/M9`、`09-26/H10,H11,H12`、`09-27/M10` | 已修（M10 收窄文档）；连带修正 2 处跨模块测试 |
| C | 关系 / 情感 / 证据 | `09-26/H20`、`09-27/M13,M14,M15`、`09-26/H21`、`09-27/M16` | 已修；复核发现 1 处邻接误伤，已补 `NON_NEGATION_TAILS` |
| D | 运维 / 安全 / 配置 | `09-26/H14,H16,H17,H18,H19`、`09-26/L9` | 已修；`try_init`/`config-check` 已接入启动路径 |
| 后续 | 其余可局部修项 | `09-26/H9,H13,H15`、`09-27/M7,M8` | 已修 |
| E | 文档收敛 | 四份历史评审消歧 | 已完成（本文件 + 4 处横幅） |
| F | MCP 分发架构 | `09-26/H7`、`09-27/M6` | 已完成（见 §2.6） |
| G | 架构：store 层下沉 | 「store 层阻塞」（同级 H7 的下一层） | 已完成（见 §2.7） |

### 2.6 批次 F 明细（2026-09-30）

| 子项 | 处置 | 落点 |
|---|---|---|
| H7：同步 `rusqlite` 跑在 async worker 上 | 新增阻塞池桥 `mcp::blocking::run`（`spawn_blocking` + `JoinError`→`Error::Internal`），17 个 handler 文件、27 个 handler 改为「薄 async 壳 + 同步自由函数」；凡 handler 内含真实 `.await`（embedding / distiller / 网络）者，仅把**同步** store/compiler/JSON 段移入闭包，await 留在壳内 | [blocking.rs](src/mcp/blocking.rs)、`src/mcp/*_tool.rs`、`src/mcp/knowledge_tools.rs`、`src/mcp/external_knowledge_tools.rs`、`src/mcp/memory_compile.rs`、`src/mcp/memory_transfer_tools.rs` |
| M6：慢工具无响应、分发无超时 | `MCPServer` 新增 `tool_timeout`（默认 [`DEFAULT_TOOL_TIMEOUT`](src/mcp/server.rs) = 60s，可经 `ServerBuilder::tool_timeout` 覆盖）；超时返回结构化 `isError: true` 结果而非丢弃响应，保持「一请求一响应」 | [server.rs](src/mcp/server.rs) |

> 遗留（**不属**本批次范围，已记入 §3）：部分 handler 的 SQL 实际位于 store 的 `async fn` 内部（tokio `Mutex` 后仍跑同步 rusqlite），
> 真正的阻塞源在 store 层；handler 层已把自身同步计算移出，store 层下沉由批次 G 收口（见 §2.7）。

### 2.7 批次 G 明细（2026-09-30）

| 子项 | 处置 | 落点 |
|---|---|---|
| store 层同步 `rusqlite` 跑在 async worker 上（H7 的下一层） | 新增中立阻塞池桥 [`crate::blocking::run`](src/blocking.rs)（`spawn_blocking` + `JoinError`→`Error::Internal`），`src/mcp/blocking.rs` 改为 `pub use` 转发（不删文件，handler 调用点零改动，且避免 storage→mcp 的分层倒置） | [blocking.rs](src/blocking.rs)、[mcp/blocking.rs](src/mcp/blocking.rs)、[lib.rs](src/lib.rs) |
| 知识库 store 的连接与锁 | `conn: tokio::Mutex<Connection>` → `Arc<std::sync::Mutex<Connection>>`（`spawn_blocking` 的闭包必须 `'static`，无法在阻塞线程内 `await` 取 tokio 锁）；新增 `with_conn(work)` 把「克隆 Arc → 同步 lock → 执行 SQL」整体放进阻塞池，**锁绝不跨 `await`**；std Mutex 毒化按 `into_inner()` 恢复（与替换前 tokio Mutex 无毒化行为等价） | [store/mod.rs](src/knowledge/store/mod.rs)、[world_io.rs](src/knowledge/store/world_io.rs)、[trait_impl.rs](src/knowledge/store/trait_impl.rs)、[queries.rs](src/knowledge/store/queries.rs) |
| `Box<dyn ToSql>` 跨线程 | `find_object_by_name` / `resolve_chapter_nos` / `search_evidence_query` / `get_evidence_for_many_query` 的参数对象补 `+ Send`，否则无法移入阻塞闭包 | [trait_impl.rs](src/knowledge/store/trait_impl.rs)、[queries.rs](src/knowledge/store/queries.rs) |
| story_bridge 的 fact-store 调用 | `bridge_story_events_to_persona` 保持 `&SqliteFactStore` 签名（不改调用点），新增 `on_blocking` 把 4 处同步调用（`resolve_agent` / `get_facts` ×2 / `insert_batch`）送入阻塞池；`SqliteFactStore` 改 `Arc<Mutex<Connection>>` + `#[derive(Clone)]`，克隆共享同一连接 | [story_bridge.rs](src/story_bridge.rs)、[fact_store/mod.rs](src/fact_store/mod.rs) |

> 语义保持：JSON 负载、错误类型、事务边界（`BEGIN`/`COMMIT`/`ROLLBACK` 仍各自独立取锁，行为与批次 G 之前一致）均未变。
> **有意收紧的两处**：`link_event_participant_row`、`upsert_world_state_row` 原先「两次上锁」合并为一次阻塞跳 + 一次锁，
> 更原子（并发写者无法在 entity upsert 与 participant/state 插入之间删除该 entity），已在代码注释中说明。

---

## 3. 未关闭清单（OPEN）

| 消歧 ID | 位置（file:line） | 问题一句话 | 严重度 | 批次 |
|---|---|---|---|---|
| 09-27/M2,M3,M11,M12 | `src/observation_compiler.rs`、`src/compiler/extract.rs`、`src/knowledge/memory_export.rs` | evidence 链未端到端、relation 无锚点、导出边去重丢时序、同名文档关联丢失 | Medium | 后续 |
| 同层残留（非本批次） | `src/store/repository.rs`、`src/store/mod.rs`、`src/character/store.rs`、`src/character/mod.rs` | 与 store 层同一形态的「`async fn` 内 tokio `Mutex` 后跑同步 rusqlite」；批次 G 范围仅限 `src/knowledge/store/*` 与 `story_bridge.rs`，故此处备案为后续 | Medium | 后续 |
| 09-26/M1–M58 | 全库 | Medium 批（未逐条复核；原报告正文已并入本文件时收敛，清单不再单独保留） | Medium | 后续 |
| 09-26/L1–L42 | 全库 | Low 批（未逐条复核；同上，清单不再单独保留） | Low | 后续 |
| 09-27/L1–L14 | 全库 | Low 批（`L9` 空 token 已修，其余含时序输出非确定、阈值缝隙等；原清单不再单独保留） | Low | 后续 |
| 复核/R5 | `src/store/tests.rs`（多处，如 :54,:60,:80,:256,:263,:305,:493,:516） | 存量测试缺 `Objective:`/`Invariants:` 文档注释（规则 §IV.1） | Low | 后续 |

### 3.1 复核结论中「不采纳」的一条

- **复核/R4（计数读路径未过滤过期行）— 不采纳**：`count_by_memory_type` / `count_for_tenant` / `counts_by_type`
  有意统计**含待回收的过期行**，因为容量配额与运维观测必须以物理占用为准；读路径（`get`/`search_*`）才做 TTL 过滤。
  这属设计意图，非缺陷，故在此备案而非修改。

---

## 4. 下一步批次

| 批次 | 主题 | 包含条目 | 目标 |
|---|---|---|---|
| 后续 | 其余 | `09-26/M1–M58`、`09-26/L1–L42`、`09-27` 其余 M/L、复核/R5、§3「同层残留」 | 按迭代清理 |

---

## 5. 本轮独立复核（2026-09-29）

对 A/B/C 批次改动做只读复核，共 5 条发现，处置如下：

| 发现 | 严重度 | 处置 |
|---|---|---|
| `relationship.rs` 单字线索按 `ends_with` 后缀匹配 → `特别喜欢` 的 `别` 被当成否定词 | Medium | **已修**：新增 `NON_NEGATION_TAILS` 词尾排除表 + 回归测试 `non_negation_tails_are_not_cues` |
| `state.rs` 复制了一份 `STANCE_FLIP_MIN_SHARED_BIGRAMS` 常量（与 `persona/timeline.rs` 重复） | Low | **已修**：改为 `use crate::persona::timeline::STANCE_FLIP_MIN_SHARED_BIGRAMS;` |
| `repository.rs::forget_expired` 用 `expires_at < ?`，而读路径与 `is_expired` 用 `<=`，恰好等于 now 的行不被回收 | Low | **已修**：改为 `<=` |
| 计数读路径缺 TTL 过滤 | Low | **不采纳**（见 §3.1） |
| `src/store/tests.rs` 存量测试缺 `Objective:`/`Invariants:` | Low | 保留为 OPEN（见 §3） |

复核确认无发现的契约：`resolver.rs` 恒等捷径与双空集、四处 `LeftmostLongest`（失败路径未变 panic）、
存储四类读路径的过期门与 LIKE 转义、hybrid 候选并集、FTS 回填、`self_disclosure` 精确 offset/length、规则合规（无 `#[allow]`）。

### 5.1 批次 G 复核（2026-09-30）

对批次 G 改动逐文件复核（`blocking.rs`、`knowledge/store/{mod,world_io,trait_impl,queries}.rs`、`story_bridge.rs`、`fact_store/mod.rs`）：

| 检查项 | 结论 |
|---|---|
| 是否有锁跨 `await` | 无。`with_conn` 的连接 guard 完全在阻塞闭包内创建并析构；`with_foreign_keys_disabled` 等组合方法每次只取一次锁 |
| 是否有同锁重入（std `Mutex` 不可重入 → 自死锁） | 无。闭包类型为 `FnOnce(&Connection) -> Result<T>` 且 `'static`，无法捕获 `self`，编译器即排除了嵌套调用 store 方法 |
| 阻塞池内是否有嵌套运行时（`block_on`） | 全库 `src/` 无 `block_on` 调用，不存在「阻塞线程等自己」的路径 |
| 事务边界是否变化 | 未变：`BEGIN`/`COMMIT`/`ROLLBACK` 仍各自独立取锁（与 G 之前一致，属既有设计）；PRAGMA/`is_autocommit` 为连接级，跨不同阻塞线程使用同一连接由 std `Mutex` 串行化 |
| 语义/负载/错误类型 | 未变（见 §2.7 尾注）；仅 `link_event_participant_row`/`upsert_world_state_row` 由两次上锁合并为一次，属收紧 |
| 性能副作用 | 每次 SQL 增加一次阻塞池调度；`entity_timeline`/`relation_graph` 的逐邻居 `get_object` 循环会放大该开销。属既有循环结构，未在本批次改变算法，备案为后续可选优化 |
| 规则合规 | 最长文件 980 行（≤1000）、`#[allow(...)]` 0 处、注释全英文、改动后 `cargo fmt --all` 已执行 |

---

## 6. 编号消歧约定与历史文件冻结

由于四份历史评审文件的编号（`C`/`H`/`M`/`L`）在各自文件内独立编号、**彼此冲突**（例如同写 `H1`，`09-26/H1` 指
`knowledge_attach` 路径穿越，而 `09-27/H1` 指单字 Jaccard 相似度 bug；`H2`/`H3`/`H5` 亦同样撞号），
**任何引用一律写成 `<报告日期>/<ID>`**（如 `09-26/H7`、`09-27/H1`），不得裸引 ID。
其中 `review-2026-09-25.md` / `review-2026-09-26.md` / `review-2026-09-27.md` 三份已在收敛时**并入本文件并移除**，
`review-fixes-2026-09-27.md` 保留在仓库中；被并入条目的正文不再单独维护，其状态一律以本文件的 CLOSED / OPEN
两节为准，本文件是唯一的权威状态与「未关闭清单」来源。