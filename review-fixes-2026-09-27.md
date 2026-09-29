# 修复记录（2026-09-27）— 原 `review-2026-09-26.md`（我那份 R1–R12）的落地情况

> 背景：我 09-26 写的评审报告（R1–R12，聚焦存储迁移 / 证据链 / 导出导入 / 蒸馏作用域）在
> **09-27 10:12 被另一处编辑整体改写**为一份范围大得多的"全库缺陷扫描"（8 Critical + 21 High）。
> 内容已无法恢复，因此把**已完成的修复**单独记录在此，避免与那份报告互相覆盖。
>
> 依据 `plan/rules/rules.md` 逐模块执行：每个模块改完即测；全程**未执行 `git`**、**未删除任何文件**。

## 1. 修复清单

| 编号 | 问题 | 修法 | 文件 |
|---|---|---|---|
| **R1** | 唯一索引迁移会让**脏库永久打不开**：去重 `DELETE` 被外键拒绝 → 建索引失败 → `open()` 失败 | 用窗口函数映射（`MAX(id)` 存活）**先把子行改指到存活行**（`UPDATE OR IGNORE` + 清掉无法改指的重复链接）再删父行，整段一个事务；**失败带索引名报错**（不再 warn-then-fail） | `src/knowledge/store/mod.rs`、新增 `tests/store_migration.rs` |
| **R10** | 索引只按**名字**判断"已安装"，同名列不同定义会被接受 → 之后所有 `ON CONFLICT` 语句失败 | 比对存储的 SQL（SQLite 存的是去掉 `IF NOT EXISTS` 的原文），定义变了就 `DROP` 重建 | 同上 |
| **R2** | 证据行的租户归属丢失：两条写路径都不写 `tenant_id` → 全部落在列默认值 `'default'`；全库无按租户过滤 | 锚点**继承事实所属实体的租户**（新增 `tenant_of_on`）；`insert_evidence` 增加 `tenant_id` 参数 | `src/fact_store/{evidence,entities,mod}.rs`、`src/mcp/provenance_tool.rs` |
| **R3** | `import_bundle` 无事务：失败留半张图，工具层却回"导入失败" | 拆出 `import_bundle_body`；整个导入一个事务；**已在外层事务中则由调用方持有**（不嵌套）；失败 `rollback` 且保留原始错误。事务 API 并入 `KnowledgeStore` trait（`&dyn` 才可用） | `src/knowledge/memory_export.rs`、`src/knowledge/store/{trait_def,trait_impl,mod}.rs` |
| **R4** | `mentions` 既不导出也不导入 → 备份/恢复静默丢失实体定位索引 | bundle 新增 `mentions` 段（v4），以 `(doc_title, object_name)` 跨库引用、按 `(span, alias)` 去重（重复导入收敛） | 新增 `src/knowledge/memory_export_mentions.rs`、`src/knowledge/{memory_export,mod}.rs`、`src/mcp/memory_transfer_tools.rs` |
| **R5** | 证据**链接**只按 content 重锚定 → 同一句出现两次会挂错位置；且结构体注释声称 span 优先（与实现相反） | 链接携带 `evidence_start/end`，导入改用 **span 优先**解析器（与世界档案同一条路径）；删掉错误注释 | `src/knowledge/memory_export_links.rs`、`src/knowledge/memory_export.rs` |
| **R6** | 容量控制**跨用户**驱逐，与同文件 T15 的"按用户隔离"不一致且无痕迹 | 保留"租户级配额"这一**声明过的意图**，把"可跨用户驱逐"写成显式契约（含与 T15 差异的说明），驱逐时 `warn!` 点名受影响用户（**已被 §6.3 修订**：多租户措辞改回单机语义，日志去掉"受影响用户"明细，行为不变） | `src/distiller/pipeline.rs` |
| **R7** | `memory_compile` 的兼容记忆/决策写入**丢弃调用方 `user_id`** → 不归属任何用户，也永远无法与该用户记忆去重 | 两个写入路径都携带 `user_id` | `src/mcp/memory_compile.rs` |
| **R8** | 导入冲突策略未声明：`ON CONFLICT DO UPDATE` 实为"bundle 覆盖" | 在 `import_bundle` 上写明契约（identity 已存在时 **bundle wins**、会覆盖旧值、如何避免），加测试钉住 | `src/knowledge/memory_export.rs`、`tests/memory_export.rs` |
| **R9** | 蒸馏跨阶段非原子：冲突阶段先删旧行（独立事务），写入阶段才插新行 → 中间失败=丢更新 | 冲突阶段**不再直接删**（返回待删 id）；写入阶段用新的 `ExperienceRepository::replace_batch` 一次事务完成"删旧+写新"；指标改为写入成功后才计数 | `src/store/{mod,repository}.rs`、`src/distiller/pipeline.rs` |
| **R11** | 贴线文件 `profile.rs` 999 行 | 单元测试移到同级 `src/compiler/profile/tests.rs`（保留私有访问，沿用 `conversation_compiler/tests.rs` 的做法）→ 658 行 | `src/compiler/profile.rs`、新增 `src/compiler/profile/tests.rs` |
| **R12** | 导入路径 `.expect`；未知枚举静默改写；未解析引用静默丢弃；三个死列无标注 | `.expect`→错误返回；未知 `object_type`/`origin` **告警**；新增 `ImportStats::unresolved_references`（含 links/mentions 的跳过计数）；死列在 DDL 标注 RESERVED | `src/knowledge/memory_export{,_links,_mentions}.rs`、`src/storage/schema.rs` |

## 2. 写测试时**新发现并修掉**的隐患（原报告的 R 清单里没有）

`list_world_events()` 在测试里直接失败：`Invalid column type Null at index: 5, name: description`。

`events.description`、`events/world_entities.importance`、`world_entity_profiles/world_relations/world_states.confidence`
**在 DDL 里可空**，却被当非空 `String`/`f64` 读取 —— **一行 NULL 就让对应的 `list_world_*` 与整条导出链永久失败**（与上一轮修掉的 `confidence = COALESCE(weight, 1.0)` 同一类）。
已按各表 DDL 默认值兜底（`0.5` / `1.0` / `0.8`，同时作为 `NewWorldEvent/NewWorldState::default()` 的单一来源），并加 `null_optional_columns_do_not_break_reads` 钉住六处读取。

## 3. 验证结果

| 检查 | 结果 |
|---|---|
| `cargo fmt --all --check` | ✅ 干净 |
| `make check` | ✅ 0 error |
| `cargo clippy --all-targets --all-features` | ✅ 0 warning |
| `make test` | ✅ **884 passed / 0 failed / 9 skipped**（修复前 869 → +15） |
| `#[allow(...)]`（规则 5） | ✅ 0 处 |
| 单文件 ≤1000 行（规则 1） | ✅ 最大 `src/store/mod.rs` **986**、`knowledge/store/tests/mod.rs` 971 |
| `mnemosyne config-check` | ✅ 未回归（419 词 / 11 动作 / 退出码 0） |

新增/迁移测试：`tests/store_migration.rs` 5 个、`tests/memory_export.rs` 10 个（原 5 个迁入 + 事务/span 重锚定/mentions 往返/bundle-wins/未解析计数）、
`memory_export_mentions` 1 个、`store::tests` 1 个（`replace_batch` 原子性）、`fact_store::evidence` 2 个（租户继承、半截 span 不伪造）、`mcp::memory_compile` 1 个（user_id 归属）。

## 4. 当时仍需拍板的两件事（**都已决定，结论见 §6**）

> 本节保留第一轮结束时的原文以便追溯。两件事都已在 09-27 第二轮按你的决定落地：
> ①"不要多租户措辞"（§6.3）、②"空 `user_id` 直接修"（§6.1）。

1. **容量配额作用域**：本轮保留"租户级"语义（你代码里的注释把它写成设计意图），只补契约说明与日志。若你要**每用户**配额（宁可放宽租户总量也不删别人的行），这是行为变更，说一声我改（含更新 `capacity_control_evicts`）。
2. **空 `user_id` 的归一化**：`memory_compile` 在响应里把空 id 显示为 `"default"`，但传给蒸馏/实体解析的是空串；`context_aware` 则先归一化再调用。三种写法并存，落到 `user_id` 列就是三批互不相同的行。要不要统一？

## 5. 与那份新报告（全库缺陷扫描）的重叠情况

新报告的下列条目**已被本轮修复覆盖**（建议核验后划掉）：

| 新报告条目 | 对应本轮修复 |
|---|---|
| H2「导出丢 `mentions`；导入无事务且静默跳过仍报成功」 | R4（mentions）+ R3（事务）+ R12（`unresolved_references` 计数） |
| C7 前半「冲突替换跨事务先删后插（丢数据）」 | R9（`replace_batch` 单事务，触发器中止测试钉住） |
| H4「重复编译每次都新增 evidence 行」 | ✅ **已修**（§6.2）：`ux_evidence_identity` + 幂等 upsert。第一轮只修了租户归属，把"是否去重"当作与 facts ADD-only 一致 —— **那个判断是错的**，evidence 是锚点，同一锚点就该是一行 |
| H6「`documents(title, source)` 无唯一索引」 | ✅ **已修**（§7.2）：`ux_documents_identity` + 幂等 upsert + 读取排序 |

新报告的其余条目**不在第一轮范围内**：

- **H3 已修**（§7.1）：FK 恢复改为 `with_foreign_keys_disabled`，任何退出路径都恢复，且不再误回滚调用方的事务。
- C1–C3（租户隔离是客户端可选参数 / 知识图谱无租户维度 / `memory_decay` 跨租户写）属架构级改动。但你已明确**这是单机 MCP、不要多租户语义** —— 所以它们不是"待实现的隔离"，而是**待按单机语义重新表述或直接删除**，需要你确认后我才能动。
- 其余 H1、H5、C4–C6、C8、H7–H21 仍未处理。

> **注意**：09-27 10:12 有另一处编辑改写了 `review-2026-09-26.md`（89KB）。若那是你自己的另一个窗口/agent，请先确认由谁负责哪些条目，否则我们会在同一批文件上互相覆盖。

## 6. 追加修复（2026-09-27 第二轮，按你的三条决定）

你的决定：① 容量配额的"多租户权衡"**不要**（这是单机 MCP）；② 空 `user_id` 归一化**直接修**；③ 重复 evidence 行**直接修**。

### 6.1 身份归一化（原"空 `user_id`"）

**问题**：同一个调用者在库里是两个身份 —— 工具把裸 `""` 传给 store，却在响应里回 `"default"`（`memory_compile` 只对响应做了归一化）；而冲突解决只比较 `user_id` 相等的行，所以这两批行**永远不会互相去重**。三套写法并存：`memory_compile` 传空串、`context_aware` 先归一化再调用、读取点一共 4 处。

**修法**：新增单一来源 `mcp::types::identity_arg(args, field)`（trim + 空白/缺失 → `DEFAULT_IDENTITY = "default"`），4 个工具入口（`memory_compile` / `context_aware` / `knowledge_tools` / `external_knowledge_tools`）统一使用；`context_aware` 里那段重复的 `norm_user`/`norm_tenant` 归一化随之删除；响应回显的就是落库用的那个值。

**测试**：`mcp::types::identity_arg_defaults_blank_values_and_trims`（缺失/空白/非字符串 → `default`，两侧空白被裁掉）；`mcp::memory_compile::omitted_user_id_is_the_same_identity_as_an_explicit_default`（省略与空白两种写法 → 同一个 `user_id`、同一个 `user_entity_id`）。

### 6.2 证据锚点身份唯一（原 H4「重复编译每次都新增 evidence 行」）

**问题**：`evidence` 表**完全没有身份**，所以编译同一段对话两次就追加一份锚点副本——表随编译次数线性增长，而被它支撑的事实没有任何变化。

**修法**：

- 身份 = `(tenant_id, IFNULL(doc_id,-1), IFNULL(chapter_id,-1), IFNULL(start_offset,-1), IFNULL(end_offset,-1), IFNULL(content,''))`，用**数据库唯一索引**强制（`ux_evidence_identity`，与仓库既有"去重归数据库管、不靠进程内检查"的约定一致）。`IFNULL` 占位是因为裸唯一索引把每个 NULL 视为互不相同，而会话锚点恰好没有 `doc_id`。
- 两条写路径（`anchor_evidence_on` / `insert_evidence`）改为幂等 upsert，并用 **`RETURNING id`** 而不是 `last_insert_rowid()` —— 后者在走 `DO UPDATE` 分支时**不会更新**，会返回本连接上一次插入的行（这正是仓库在 world 模型里已经踩过的坑）。
- **老库自动修复**：索引装不上去正是因为它要去重的那些重复行，所以安装前先收敛（重复组保留最新行，并**先把 `facts.evidence_id` 改指过去**再删——直接删会被外键拒绝，而那次拒绝会让 `open()` 整个失败）。老库还缺 `start_offset`/`end_offset` 列，安装前先补列（SQLite 拒绝在缺列上建索引）。
- **顺带修掉一个真缺陷**（测试当场抓到）：调用内缓存 key 原来只有 `text + span`，**不含 tenant 也不含 doc_id** —— 同一句话在不同租户/不同文档下会复用同一行，属于静默串链，不只是"省一次查询"。

**测试**：`fact_store::evidence::anchoring_the_same_span_reuses_the_row`（同锚点两次 → 同一 id、表里 1 行；不同 span / 不同 tenant 各自独立）；`fact_store::tests::duplicate_evidence_rows_are_collapsed_and_facts_repointed`（造重复行 + facts 引用 → 重开修复 → 每组 1 行且 facts 全部改指保留行）。

### 6.3 容量配额：保留租户级，去掉多租户措辞（原 R6）

**按你的决定不动行为**：单机 MCP 下 tenant 就是这台安装本身，配额语义是"这台机器留多少条记忆"，驱逐取置信度最低的行。上一轮我写的那段"跨用户权衡"的确把多租户问题带进来了，已改写为单机语义（并说明冲突解决为何更严格：**那里丢的是某个用户唯一的一条决策记录，而不是一条普通的旧行**）。日志去掉了"受影响用户"明细，改为报 `cap` 与 `evicted` 条数。

### 6.4 顺带的两处工程收敛

| 事项 | 说明 |
|---|---|
| 唯一索引安装机制提取共享 | `UniqueIndex` + `ensure_unique_index` + 去重修复从 `knowledge/store/mod.rs` 移到 `src/storage/unique_index.rs`（170 行），knowledge store（world 模型身份）与 fact store（evidence 身份）共用一份；`knowledge/store/mod.rs` 469 → 324 行 |
| `fact_store/mod.rs` 脱离红线 | 加完索引与测试后正好 **1000 行**，单元测试移到 `src/fact_store/tests.rs`（沿用 `conversation_compiler/tests.rs` / `compiler/profile/tests.rs` 的拆法）→ 576 行 |

### 6.5 验证结果

| 检查 | 结果 |
|---|---|
| `make test` | ✅ **888 passed / 0 failed / 9 skipped**（本轮再 +4） |
| `make check` / clippy | ✅ 0 error / **0 warning** |
| `cargo fmt --all --check` | ✅ 干净 |
| `#[allow(...)]`（规则 5） | ✅ 0 处 |
| 单文件 ≤1000 行（规则 1） | ✅ 最大 `src/store/mod.rs` 986 |
| `mnemosyne config-check` | ✅ 未回归（退出码 0） |

**仍未处理的一项**（等你确认，属另一份报告的范围）：`review-2026-09-26.md`（被改写后的"全库缺陷扫描"）里的 C1–C3 等架构级条目，以及 H3 / H6；我上一轮建议先做 H3、H6，你说一声即可。

## 7. 第三批修复（2026-09-27，H3 / H6 + 两个新发现的缺陷）

按"继续做吧"，处理 `review-2026-09-26.md` 的 **H3** 与 **H6**。过程中另有两个**新发现的缺陷**（不在任何报告里）一并修掉。

### 7.1 H3 · `?` 提前返回 → `PRAGMA foreign_keys` 永久 OFF

**问题**：`clear_all`、`clear_for_document`、`Migrator::migrate` 三处是同一形状：
`禁用 FK → BEGIN → 干活 → COMMIT → 恢复 FK`。而 `BEGIN` 或 `COMMIT` 上的 `?`
（`is_autocommit` 检查与 `BEGIN` 之间有窗口、`SQLITE_BUSY`、磁盘满）会在**恢复 pragma 之前**返回 ——
此后这条连接的每次写都在参照完整性关闭的状态下进行，正是那段 pragma 逻辑想避免的事。
更隐蔽的是：`PRAGMA foreign_keys` 在事务内是 no-op，所以连"记得恢复"的路径在 commit/rollback 自身失败时也是空操作。

**修法**：三处共用一个 `SQLiteKnowledgeStore::with_foreign_keys_disabled(work)`：

- `BEGIN` 前关 FK，**任何退出路径**都恢复（不再依赖调用方记得）；
- 只在 **autocommit** 状态下恢复；事务因 commit/rollback 失败而残留时先补一次 `ROLLBACK`（并告警）；
- 恢复失败返回恢复错误（比工作错误更严重：连接在重开前都不安全），两者都失败则记 `error!` 并返回工作错误。

### 7.2 H6 · `documents(title, source)` 无唯一身份

**问题**：所有写路径都是"两次独立加锁：`find_document` 没有 → `create_document`"，
两个编译同一部作品的调用者可以都读到 `None`、都插入 → 同一身份两行；
对象/边/证据随即分裂到两行上，`clear_for_document` 只清一半（重迁移就复制一遍图），`graph_counts` 还会重复计数。
`world_entities` / `events` / `world_states` 早已是唯一索引 upsert，documents 是唯一漏网的。

**修法**：新增 `ux_documents_identity(title, source)`（复用 §6.4 提取的共享安装器）；
`create_document` 改幂等 upsert 并用 `RETURNING id`（`DO UPDATE` 分支不更新 `last_insert_rowid()`）；
冲突分支用 `COALESCE` 只补不抹（不提供 author 的调用者不能抹掉已记录的 author）；
`find_document` 补 `ORDER BY id ASC`（与 `find_document_by_title` 的既有理由一致）。
老库重复行在 `open()` 时收敛到最新行，`chapters`/`knowledge_objects`/`evidence`/`compiler_runs` 四张子表全部改指过去。

### 7.3 新发现 ①：DDL 的时间戳默认值**恒为 NULL**

修 H6 的迁移测试时 `find_document` 直接失败：`Invalid column type Null at index: 5, name: created_at`。

**根因**（sqlite 实测）：`strftime('%s','localtime')` —— `localtime` 是**修饰符**，
放在"时间值"的位置时整条表达式求值为 **NULL**：

```
SELECT strftime('%s','now')             → 1790476810
SELECT strftime('%s','localtime')       → None        ← 就是这个
SELECT strftime('%s','now','localtime') → 1790505610  ← 正确写法
```

**影响**：`src/storage/schema.rs` **9 处** DDL 默认值都是它 → 凡未显式给时间戳的插入都写 NULL；
而 `row_to_document` / `row_to_object` / `row_to_edge` / `row_to_evidence` 把 `created_at` 当**非空 `i64`** 读
→ **一行 NULL 就让对应读取永久失败**（与 §2 记录的 world 表 NULL 同类）。
另外 `world_io.rs` 有 **2 处** upsert 用它，等于把 `updated_at` 显式写成 NULL。

**修法**：9 处 + 2 处改为 `strftime('%s','now')`（UTC 秒，与 Rust 侧 `Utc::now().timestamp()` 一致；
`localtime` 反而会带时区偏移）；4 个读取点改为容忍 NULL（"时间未知" → `0`），
这样**老库里已经写坏的 NULL 行仍可读**（不回填、不伪造时间）。

### 7.4 新发现 ②：`with_foreign_keys_disabled` 第一版会**误回滚调用方的事务**

这是修 H3 时我自己引入、被新测试当场抓住的：当 `BEGIN` 失败（连接已在调用方事务中，
`migrate`/`clear_all` 正是这种用法）时，第一版的"事务还开着就先 `ROLLBACK`"逻辑
把**调用方的事务**回滚了 —— 测试里表现为随后的 `rollback_transaction` 报
`cannot rollback - no transaction is active`。
**修法**：由 `BEGIN` 成功与否决定"这个事务是不是我们的"，只有自己的事务才会被补回滚。

### 7.5 顺带：`knowledge/store/tests/mod.rs` 越过规则 1 的红线

加完 H3/H6 的测试后该文件 **1099 行**。按既有拆法拆出 `tests/objects.rs`（453）与
`tests/world.rs`（225）→ `tests/mod.rs` **421 行**（helper 留在父模块，子模块 `use super::*`）。

### 7.6 验证结果

| 检查 | 结果 |
|---|---|
| `make test` | ✅ **893 passed / 0 failed / 9 skipped**（本轮 +5：FK 恢复、documents 幂等、documents 迁移修复、时间戳默认值、NULL 时间戳可读） |
| `make check` / clippy | ✅ 0 error / **0 warning** |
| `cargo fmt --all --check` | ✅ 干净 |
| `#[allow(...)]`（规则 5） | ✅ 0 处 |
| 单文件 ≤1000 行（规则 1） | ✅ 最大 `src/store/mod.rs` **986**（本轮把 1099 行的测试文件拆掉） |
| `mnemosyne config-check` | ✅ 未回归（退出码 0） |

**核验**：§1/§2 声称的 R1–R12 落地痕迹逐项抽查存在（`unresolved_references`、`replace_batch`、
`ensure_evidence`、`import_bundle_body`、`tenant_of_on`、`ImportStats`、`memory_export_mentions`），无"文档说修了但代码里没有"的情况。

**仍未处理**：C1–C3（属架构级，且按"单机 MCP、不要多租户"的定位应**重新表述**而非实现）、
H1、H5、C4–C6、C8、H7–H21。

## 8. 第四批修复（2026-09-27，C1 / C2 / C3 / C5 / C6）

按"继续做吧 + 严格按编码规范 + 不要多租户（单机 MCP）"推进 `review-2026-09-26.md` 的 Critical。

### 8.1 先定基线：本引擎是**单机**服务（C1 / C2 / C3 的统一收口）

三条 Critical 的前提都是"多租户隔离"，而你的定位是**单机 MCP**。所以按报告 C2 给出的第二个选项处理：
**不做租户隔离，消除"看起来隔离了"的假象**，并在文档里明确声明。

| 编号 | 原问题 | 单机语义下的修法 |
|---|---|---|
| **C1** | `ensure_entity_tenant` 在调用方没传 `tenant_id` 时直接 `Ok(())` —— 校验只是**看起来**存在；不传就能靠猜 id 读别人的 `state_timeline` / `fact_provenance` / `decision_trace` / `decision_search` / `persona_timeline` / `memory_feedback` | 删除"可选"这一层：租户一律经 `mcp::types::identity_arg` 归一化（**省略 = 本地标签**），校验**恒定执行**。`tenant_argument` 随之删除（调用点直接复用 `identity_arg`，消掉第二份实现） |
| **C2** | `compile_source` 的 `_tenant_id` 是装饰参数（函数体从不使用），调用方以为图谱按租户隔离 | 删掉该参数与 `generalize_compile` 的 `tenant_id` 声明（9 个调用点一并更新）。知识图谱按设计**不分区**，两处 README 新增「部署模型：单机」章节明确写出 |
| **C3** | `memory_decay {entity_id}` 不带 `tenant_id` 时跳过归属校验，`run_decay_pass` 的 `Some(id)` 分支也不过滤 → **跨标签归档**（这是写操作） | 只要给了 `entity_id` 就**无条件**校验归属；`memory_feedback` 的"可选绑定"同样改为恒定绑定 |

**行为变化（有意为之）**：以前"省略 `tenant_id` 就等于不校验"，现在"省略 = 本地标签"。因此**标签不符的 id 现在报 NotFound**。
受影响的测试全部按新契约重写（两个 `..._when_asked` 测试改名 `..._enforce_tenant_always` 并断言"省略也必须拒绝"；`cognitive_state_e2e` 的 11 处读取补上它们编译时用的标签；两个用裸 `entity_id: 7` 的测试改为先建实体）。

### 8.2 C5 · 承诺上限「字符 vs 字节」不一致 → 长中文消息中断整个 compile

`validate_decision` 用 `String::len`（**字节**）比较 512，而提取器用 `truncate_chars`（**字符**）截断：
512 个汉字的 object = 1536 字节 → 校验必失败 → 因为 `insert_compilation` 在**开事务之前**校验全部决策，
**一条长中文承诺就让整轮编译一条 fact 都不落库**，客户端只看到 `invalid decision field 'object'`。

**修法**：两侧统一为**字符**，并把上限收敛到单一来源（`decision::MAX_OBJECT_CHARS` / `MAX_VERB_CHARS`，
`commitment.rs` 改为引用它 —— 两份常量正是漂移的根源）。补了缺失的**跨模块契约测试**：
提取器的输出必须能被写校验接受（原先两个模块各测各的，中间没有断言）。

### 8.3 C6 · 衰减读取**没有任何生产者写入**的 payload key

`importance` / `access_count` 都从 fact payload 读，而生产构造器（`persona::signal_to_fact`、会话/观测编译器、
`agent_facts`、`commitment::anchor_fact`）**一个都不写** → `ImportanceBased` 恒不下档、`AccessFrequencyBased` 恒不下档、
`Hybrid` 退化成时间策略。最危险的是：运维把 `importance_threshold` 从默认 0.5 调到 0.6（一个看起来很合理的值），
**每条未受保护的事实**都满足 `decay_score < 1.0` → 一次后台 tick **全库降权归档**。

**修法**：`extract_importance` 改为 —— payload 里显式写了 `importance` 就用它，否则回落到 **`Fact::confidence`**
（生产者真正在填的信号，来自 persona 信号分数）。`access_count` 保持读 payload（无写入方 → 恒 `None` → 不降权，
方向是安全的），并在读取处写明"目前没有生产者"。

### 8.4 验证结果

| 检查 | 结果 |
|---|---|
| `make test` | ✅ **895 passed / 0 failed / 9 skipped**（本轮 +2 净增：新增 3 条、重写 8 条） |
| `make check` / clippy | ✅ 0 error / **0 warning** |
| `cargo fmt --all --check` | ✅ 干净 |
| `#[allow(...)]`（规则 5） | ✅ 0 处 |
| 单文件 ≤1000 行（规则 1） | ✅ 最大 `src/store/mod.rs` 986 |
| `mnemosyne config-check` | ✅ 未回归 |

**下一批**：C4（http session 生命周期）、C7（冲突替换的另一半：平分重复被保留）、C8（中文 + embedding 零结果）、
H1（`knowledge_attach` 的路径穿越）、H5（AhoCorasick 匹配模式），以及其余 H 条目。

### 8.5 C8 · 中文查询在 embedding 模式返回**零结果**

`tokenize` / `bm25_score` 按"非字母数字"切分，而汉字是字母数字且词间无空格 →
`「刘备很高兴。」` 整体是**一个 token**，查询词永远匹配不上 → `bm25_score == 0` →
`repository.rs` 把 0 分行直接 `continue` 丢弃：`dim > 0` 时**结果集为空**（报告症状），
`dim == 0` 时 LIKE 兜底能召回但每行得分都是 0，排序退化成 importance。

**修法**：对 Han 连续段做**重叠字符 bigram**（查询侧与文档侧同一套规则，所以两字查询词就是文档产出的那个 bigram）；
单字段保留原样（否则单字查询永远匹配不到）；`bm25_score` 不再自己 split，直接复用 `tokenize`（消掉第二套切分规则）。
补了 tokenize 单元测试、BM25 中文（有匹配 / 无匹配 / 匹配更多词得分更高），以及 **`dim == 0` 与 `dim > 0` 两个分支**的端到端检索测试。

### 8.6 H5 · AhoCorasick 默认模式让 `not` / `began*` / `Mrs` **永不匹配**

默认 `MatchKind::Standard` 报"最早结束"的匹配 + `find_iter` 非重叠：`no`/`not` 共起点时先报 `no`，
游标越过该区间，词边界过滤又把 `no` 丢掉 —— `not` **再也没有机会被尝试**。随包词典里有 **131 组**这样的前缀对，
受害者包括 `not`（被 `no` 吞）、`began/begin/begins/beginning/begun`（被 `be` 吞）、`Mrs`/`Mrs.`（被 `Mr` 吞），
而这个 matcher 正是会话编译器的**英文否定检测**与 title/entity 扫描所依赖的。

**修法**：改用 `MatchKind::LeftmostLongest`（同位置取最长形态）。补两条测试：更长形态可达（`I do not care` 必须匹配 `not`）
+ 边界约束没有被放松（`notebook` 里的 `not` 仍须拒绝）+ `no` 单独出现时仍匹配（不能换来换去）。

### 8.7 H1 · `knowledge_attach` 沙箱可读引擎自身配置与数据库

原实现有两个缺口：

1. **白名单就是整个安装根**。`config/`、SQLite 文件、部署凭据都在里面，`knowledge_attach {"path":"config/…"}` 可以直接读出来，
   再经 `evidence` 工具取回 —— 这不是"附加语料"，而是"读取主机任意文件"的原语。
2. **只比较路径文本**。`starts_with` 比的是字符串，白名单目录里放一个 symlink（`root/link -> /etc`）就能过检查，
   而实际读取会跟着链接走出去。

**修法**：应用白名单**之前**先 `canonicalize`（两侧都规范化，否则 macOS 的 `/var` 与 `/private/var` 会误杀合法文件）；
`config/…`、`.env*`、`*.db`/`*.sqlite*` 明确拒绝；对"扩展名未知 → 兜底当文本"的情况，
读头部 8KB 检查 NUL 字节（`notes.bin` 这类必须被拒，`.txt` 与 PDF 各自按声明豁免）。
`memory_transfer_tools::resolve_transfer_path` 同样加固（解决"导出目标尚不存在"的场景：只 canonicalize **已存在的前缀**）。

### 8.8 顺带

`src/store/mod.rs` 因新增测试涨到 **1024 行**（越规则 1 红线），按既有拆法把测试移到 `src/store/tests.rs`
（`mod.rs` 424 + `tests.rs` 608）。

### 8.9 验证结果（第四批）

| 检查 | 结果 |
|---|---|
| `make test` | ✅ **904 passed / 0 failed / 9 skipped** |
| `make check` / clippy | ✅ 0 error / **0 warning** |
| `cargo fmt --all --check` | ✅ 干净 |
| `#[allow(...)]`（规则 5） | ✅ 0 处 |
| 单文件 ≤1000 行（规则 1） | ✅ 最大 `src/ingest/characters/data.rs` 950 |
| `mnemosyne config-check` | ✅ 退出码 0 |

**已关闭**：C1、C2、C3、C5、C6、C8、H1、H3、H4、H5、H6（含此前记在 §6/§7 的条目）。
**仍未处理**：C4（http session 生命周期）、C7 的"平分重复被保留"那一半、H7–H21。

## 9. 第五批修复（2026-09-27，C4 / C7 后半 / H8）

### 9.1 C4 · HTTP 会话隔离靠客户端自封 + 无会话走全局广播

**先纠正一个前提**：我此前以为 HTTP 传输是 0.1.3 移除 `--sse-addr` 后留下的死代码 —— **不是**。
`main.rs:703` 在 `--transport http` 下调用 `serve_http_addr`，所以这是**真实可达**的网络暴露面。

**问题（两条独立缺陷）**：

1. 服务端**从不签发** session id，也**从不校验**：任何字符串都会 `entry().or_insert_with(...)` 新建频道，
   于是猜到别人的 id（`"1"`、`"default"`）就能订阅到别人的全部响应；而规范拼写 `Mcp-Session-Id`
   **根本没被读取**，被静默当成"无会话"。
2. 两个客户端都**不带**头时，所有响应进全局广播 → 两个 SSE 订阅者**互相收到对方响应**。

**修法**：

- `initialize` 由服务端**签发** `uuid::Uuid::new_v4()`，放在响应头 `Mcp-Session-Id`（兼容读取旧拼写 `x-mcp-session-id`）；
- `POST /message` 与 `GET /sse` 都**强制**：没带 → 400，带了但**不是服务端签发的** → 404；
- **删除全局广播通道**（`AppState.response_tx` / `HttpTransport.response_tx` 一并移除），无会话的响应改为**丢弃 + warn**，
  而不是扇出给所有订阅者；
- `message_handler` 拆成"裁决会话 → `forward_message` 转发 → 回写签发头"三段，界限清晰可测。

**行为变更**：HTTP 客户端现在必须 `initialize` 一次并携带返回的 id（两处 README、`docs/{zh,en}/mcp.md`、
`docs/{zh,en}/architecture.md` 已同步）。

### 9.2 C7 后半 · 平局重复被保留（堆数据）

R9 已经修掉"跨事务先删后插"（`replace_batch` 单事务）。剩下的是另一半：`resolver` 比较 importance，
"新的不严格大于旧的"就 `KeepBoth` —— 而蒸馏是**确定性**的，重复蒸馏同一批对话得到**字节相同**的内容与相同 importance，
于是**每轮多插一份**，无限增长。而同一函数的 keyword 路径对同样的平局**是丢弃**的（两条路径行为相反）。

**修法**：**同类型 + 内容字节相同**一律 `ReplaceOld`（它就是同一条记忆）✓ 两条路径语义一致。
补两条测试：`resolver` 单元测试（平局/更低 importance 都替换，且旧行 id 正确）+ 端到端
`repeated_distillation_does_not_grow_the_store`（跑两轮，记忆数不变）。

### 9.3 H8 · session map 永不回收（内存耗尽）

与 C4 同一处代码：客户端断开时没有任何清理，每个离场客户端给进程留下一个 1024 槽广播频道。

**修法**：`SessionEntry { sender, last_seen }` + `SESSION_TTL`（30 分钟），每次会话操作时惰性回收
（`evict_stale_sessions`），并加纯逻辑单元测试（过期被清、新的保留）。

### 9.4 顺带的两处拆分

| 文件 | 原因 | 结果 |
|---|---|---|
| `src/mcp/http_server.rs` | 加完会话逻辑后 962 行贴线 | 589 行 + `mcp/http_server_tests.rs`（610） |
| `src/distiller/mod.rs` | 加完 C7 测试后 989 行贴线 | 230 行 + `src/distiller/tests.rs`（764） |

HTTP 测试同时按新契约重写（签发 / 缺失 400 / 未知 404 / 空闲回收），并修掉一个被旧全局广播掩盖的真实测试缺陷：
`post_message_returns_response_body_without_sse` 里 handler 与 transport 用**两个不同的 session map**。

### 9.5 验证结果（第五批）

| 检查 | 结果 |
|---|---|
| `make test` | ✅ **908 passed / 0 failed / 12 skipped** |
| `cargo test --lib -- mcp::http_server --include-ignored` | ✅ **13 passed**（8 个 `#[ignore]` 的 HTTP 用例全部实跑通过） |
| `make check` / clippy | ✅ 0 error / **0 warning** |
| `cargo fmt --all --check` | ✅ 干净 |
| `#[allow(...)]`（规则 5） | ✅ 0 处 |
| 单文件 ≤1000 行（规则 1） | ✅ 最大 `src/ingest/characters/data.rs` 950 |

**已关闭**：C1–C8（全部 Critical）、H1、H3、H4、H5、H6、H8。
**仍未处理**：H2 的若干子项已修（见 §5）、H7、H9–H21。
