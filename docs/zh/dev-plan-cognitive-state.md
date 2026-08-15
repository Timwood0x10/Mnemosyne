# Mnemosyne v0.3 开发计划：从 Memory System 到 Cognitive State System

> 状态：**v3 定稿（2026-08-15，架构冻结，可开工）** · 对应版本：v0.3.0 / v0.3.1
>
> 借鉴对象：[semantica-agi/semantica](https://github.com/semantica-agi/semantica)（Graph-Native Infrastructure for Context and Accountable AI Systems）。
> 只吸收其 **Provenance → Temporal → Conflict → Causal → Decision** 五种思想，并将其重解释为
> **Evidence → Cognitive State → State Transition → Relationship → Agent Action**。
>
> v3 冻结结论：不做过度设计。只吸收思想，不吸收技术栈；不建 Knowledge Graph / Ontology / Reasoner / Causal Engine / Event Store / LLM Judge。

---

## 1. 背景与目标

### 1.1 核心架构原则（冻结）

> **Fact 不是最终产物，Fact 是 Cognitive State 的证据单元。**

v0.3 只跑通四个词：

```
Evidence → State → Transition → Decision
```

Mnemosyne 构建的不是 **World Model**，而是 **Human State Model**。

### 1.2 架构 DNA（五句话，冻结）

- **Facts preserve experience.** 事实保存经验。
- **Evidence justifies facts.** 证据为事实提供依据。
- **State interprets facts.** 状态解释事实。
- **Transition explains change.** 过渡解释变化。
- **Decision records action.** 决策记录行为。

### 1.3 版本定义

- **v0.3.0**（核心升级）：`Fact → Evidence → Current State + Historical State + Transition`
  - 解决：What do we know? / Why do we know it? / What was true before? / How did it change?
- **v0.3.1**（实验性）：再加 `Decision → Because → Evidence → Outcome`
  - 解决：Why did the agent act this way? / What happened afterwards?

---

## 2. 核心 Invariants（测试原则，非新功能）

### 2.1 State 是 derived view，不是 replacement storage（最重要）

```
Fact Store
     ├── F1
     ├── F2
     ├── F3
     └── F4
          │
          ▼
      StateEngine
          │
          ▼
      StateInterval
```

- **任何新的 State 都不能让原始 Fact 消失。**
- `latest_by_payload_key()` 哪怕继续存在，也只是 current-state **projection**，历史事实永远在。
- **State can be recomputed from Facts.** —— StateEngine 算法将来变了，old Facts + new StateEngine → new State 依然可重算。
- 这正是本计划不需要 Event Sourcing 的原因：**Fact 本身已经承担了足够的历史基础。**

### 2.2 时间语义（写死，防 Temporal 悄悄搞错）

- `Fact.time` = **observation time**（观察到的时间）
- `StateInterval.from/to` = **state validity time**（状态生效时间）

两者可以不同。例：2026-08-15 说"我从去年开始喜欢 Rust" → observation time 2026-08-15，state valid_from ≈ 2025。
**当前不做完整 bi-temporal model**，只需在代码注释中写死上述语义。
**不新增字段**：如果现有数据没有可靠 valid time，就退化成 observation-derived interval，不要假装精确。

### 2.3 正交性原则

`confidence ≠ status ≠ decay`，三者必须保持独立。`status` 不承担 confidence / decay / expiration 的职责。

---

## 3. 核心模型定义（v0.3 冻结）

### 3.1 FactStatus —— 严格三态，不再加

```rust
enum FactStatus {
    Active,        // 当前仍然有效
    Superseded,    // 被新的状态取代（Python → Rust）。旧的曾经为真，现在被新状态取代
    Contradicted,  // 存在明确冲突，但不能证明哪个为真（喜欢独处 vs 喜欢热闹）
}
```

三种完全不同的 epistemic 状态：
- **Superseded** = 时间演化（旧的曾为真，现被取代）
- **Contradicted** = 当前证据不足以确定哪个描述完整
- 冻结为三态。不加 Expired / Archived / Pending / Uncertain / Rejected。

### 3.2 derived_from —— 仅推导链，不是因果

```rust
derived_from: Vec<i64>   // 只表达：这个 Fact 基于哪些 Fact 推导出来的
```

- 语义严格限定为 **Derivation / Provenance Chain**。
- `F1(压力大) → F2(情绪低落)` 只记录推导关系，**不擅自记录因果**（那是因果判断，超出认知建模边界）。
- 未来若真需要 causal，再单独引入 `causes` / `caused_by`。现在不要。

### 3.3 StateInterval 与 StateTransition —— 两个独立结构体，Transition 不复制 Interval

```rust
struct StateInterval {
    from: i32,                  // state validity time（见 §2.2）
    to: Option<i32>,            // None = 持续到现在
    value: serde_json::Value,
    evidence_ids: Vec<i64>,
}

// Transition 是 interval 之间的关系，不要复制整个 Interval
struct StateTransition {
    from_index: usize,          // 引用 intervals 数组的下标（不引入持久化表）
    to_index: usize,
    at: i32,                    // transition 发生时间
    transition_type: TransitionType,
    evidence_ids: Vec<i64>,
}
```

- **`from_state`/`to_state` 存下标/引用，而不是完整复制 Interval**（避免重复数据）。
- 这只是纯 Rust 结构体，不引入任何 graph 抽象，也不需要 StateIntervalId 持久化表。

### 3.4 TransitionType —— 只保留三种

```rust
enum TransitionType {
    GradualChange,            // A → A' → B，连续变化
    StanceFlip,               // 喜欢 X → 不喜欢 X，明确反转
    BehavioralConfirmation,   // 说想社交 → 真的参加活动，认知状态被行为证据确认
}
```

- **砍掉 TurningPoint**——它容易退化成主观标签。以后真需要再加。
- **Transition 检测允许"不确定"**：宁可没有 transition，也不要 hallucinate transition。

---

## 4. 实施计划

### Step 1 — Fact 升维：Provenance 与生命周期（P0，约 1 周）

**目标**：每条事实都能回答"为什么这么认为"和"这条还成立吗"。

**改动**：

1. `Fact` 结构体扩展（`src/cognition.rs`）：
   - `confidence: f64` ← 映射表里已有的 `weight`
   - `derived_from: Vec<i64>`（仅推导链，见 §3.2）
   - `status: FactStatus` ← 语义化现有的 `archived`（三态，见 §3.1）
2. schema 迁移（`src/fact_store.rs` CORE_SCHEMA）：
   - `facts` 表加列 `derived_from TEXT`（JSON 数组）；`archived INTEGER` 演化为 `status TEXT`
   - **旧列 `weight`/`archived` 不删**：v0.3 采用双读/迁移（old DB → migration → confidence/status → new code），等未来 major version 再清理
   - 沿用 `CREATE TABLE IF NOT EXISTS` + 幂等 ALTER 的迁移模式
3. 兼容保障：`latest_by_payload_key` 与所有现有写入路径不动，新字段全默认值——**本步不破坏任何现有测试**
4. 新 MCP 工具 `fact_provenance`：入参 `fact_id`，返回 `{ 事实, 证据原文, derived_from 链, confidence, status }`

**涉及文件**：`src/cognition.rs`、`src/fact_store.rs`、`src/error.rs`、`src/mcp/`

**验收**：

- `make check` 0 error 0 warning、`make test` 全绿（现有 700+ 测试不回归）
- 新建单测：`derived_from` 链可递归解析；`status` 迁移后旧数据默认 `Active`
- `fact_provenance` 对编译产物返回完整溯源链

---

### Step 2 — State Transition 层：Temporal 状态区间（P1，约 1.5 周）

**目标**：`StateEngine` 增加 `aggregate_intervals()`，回答"How did the current state emerge?"。

```
StateEngine
├── aggregate()            // What is the current state?（现有，签名不变）
└── aggregate_intervals()  // How did the current state emerge?（新增）
```

**改动**：

1. 新增 `StateInterval` / `StateTransition`（见 §3.3），按语义 key 分组、按 `time` 排序产出区间序列（不再用 latest-wins 压扁历史）。
2. `StateEngine` 新增 `aggregate_intervals(facts)`：保留现有 `aggregate()` 不动（兼容）；`EntityState` 增加 `state_intervals` 字段（可选）。

   **`state_intervals` 必须是泛型 `Vec<StateInterval>`，不要演化成 per-dimension 结构体**：

   ```
   EntityState
   ├── current state      // 原有五维，不动
   └── state_intervals    // optional historical view（泛型 Vec<StateInterval>）
   ```

   就停在这里。绝不给每个 dimension 做特殊算法（否则 StateEngine 会变成 cognition god object）。

3. **Transition 检测 = Deterministic Transition Detector**：
   - 复用现有 stance-flip 检测（shared-bigrams 逻辑，`persona/timeline.rs`）作为信号源
   - **允许"不确定"**：无法建立确定关系时，只输出区间，不强行生成 transition
   - **禁止** LLM 做 state judge / transition judge
4. 新 MCP 工具 `state_timeline`：入参 `entity` + 可选 `dimension`，返回状态演化链（区间 + 证据 + transition）。

**涉及文件**：`src/cognition.rs`（StateEngine）、`src/state.rs`（新增，放 `StateInterval`/`StateTransition`）、`src/persona/reconciler.rs`、`src/persona/timeline.rs`、`src/mcp/`

**验收**：

- 用"宅家 → 想社交 → 第一次参加线下活动"测试语料：`state_timeline` **必须**保留三个时间状态及各自 evidence；**不要求**必须识别成 `GradualChange`/`BehavioralConfirmation`——识别出就输出，识别不出就只给区间
- 回归：现有 `cognitive_context` / `persona_timeline` 输出与升级前一致
- `make test` 全绿

---

### Step 3 — Decision 一等公民：认知闭环（v0.3.1 实验性，约 1.5 周）

**定位**：Decision 是 v0.3.1 的"实验性能力"——把 Mnemosyne 从 Cognitive State System 推向 Cognitive Agent Runtime，属于下一层。

**决策（已拍板）**：**独立 `decisions` 表**，不要 `FactType::Decision` 二选一摇摆。原因：Decision 有天然不同的生命周期（`made_at` / `because` / `outcome` / `status`），其中 `because`、`outcome` 不是普通 Fact 的属性。

**模型**：

```rust
struct Decision {
    id: i64,
    subject: String,
    verb: String,
    object: Option<String>,
    made_at: i32,
    because: Vec<i64>,              // 依据的事实 id（supporting evidence，不是 causality）
    outcome: Option<DecisionOutcome>,  // 创建时允许为空
    status: DecisionStatus,
}
```

**改动**：

1. `decisions` 表 + `DecisionOutcome`/`DecisionStatus` 枚举。
2. 新 MCP 工具（就两个，不扩展）：
   - `decision_trace(name)`：回溯决策依据（`because` 指向的 facts + 证据）
   - `decision_search(query)`：复用现有检索层找相似历史决策
3. **与 memory_decay 的关系（v3 冻结修改）**：
   - **v0.3.1 不修改 `memory_decay` 语义。**
   - Decision 暂按独立实体生命周期处理，**是否纳入 decay 留待后续根据实际数据决定**，现在不提前设计。

**涉及文件**：`src/storage/schema.rs`、`src/decision.rs`（新增）、`src/retrieval.rs`、`src/mcp/`

**验收**：

- 编译含"用户承诺 X / agent 答应 X"的对话，`decision_trace` 回溯到依据 facts；`decision_search` 命中同主题历史承诺
- `outcome` 在创建时为空、后续可补填
- `memory_decay` 相关测试不受影响（语义不变）

---

## 5. 风险与回归策略

| 风险 | 缓解 |
|---|---|
| Step 2 改动 `StateEngine` 影响 `cognitive_context` 输出 | 现有 `aggregate()` 签名不变，`state_intervals` 为增量字段；先写"区间聚合结果 == 现有 latest-wins 结果"的对照测试 |
| schema 迁移破坏旧库 | 沿用幂等迁移 + 全默认值；`weight`/`archived` 旧列保留，双读迁移，major version 再清理 |
| `weight`/`archived` 语义化与 `memory_decay` 冲突 | decay 模块同步改读新字段，保持行为一致；`confidence ≠ status ≠ decay` 正交 |
| transition 检测过度生成 | 允许"不确定"；无法建立确定关系时只输出区间，不强行生成 transition |
| `state_intervals` 演化成 god object | 冻结为泛型 `Vec<StateInterval>`，不给 per-dimension 特殊算法 |

---

## 6. 坚决不做（scope 红线）

**功能层面：**

- ❌ Ontology（本体治理）
- ❌ Graph database / 多后端存储（Neo4j/FalkorDB/RDF）
- ❌ Generic graph abstraction
- ❌ Causal inference engine（`derived_from` 仅推导链，`causes` 另行引入）
- ❌ Event sourcing 框架
- ❌ Rule engine / Rete / Datalog
- ❌ LLM state judge / transition judge
- ❌ Embedding-based transition detector
- ❌ 通用 Decision Graph（一个表 + 两个 trace API 足够）
- ❌ 再抽象一个 `CognitiveGraph` 图层
- ❌ per-dimension `StateIntervals` 结构体（保持泛型）

**尤其这两条**：
1. 不创建 `CognitiveGraph`。已有 `Fact` / `StateEngine` / `StateInterval` / `StateTransition` / `Evidence` / `Decision` 通过 ID 关系自然形成结构，不需要为"认知图"这个名字再抽象一层。
2. 不创建 per-dimension state 结构。`EntityState` = current state（原有五维）+ `state_intervals`（泛型），到此为止。

**架构层面：**

- ❌ RDF / OWL / SPARQL / SKOS / SHACL
- ❌ Graph Analytics、企业 ingestion（Databricks/Snowflake）
- ❌ LLM 进主链路：LLM 只保留"可选高级语义器"定位（类似现有 `fastembed` optional feature），主线确定性、零 API key 不变

---

## 7. 最终架构图（v0.3 冻结）

```
                  Conversation
                       │
                       ▼
                  Observation
                       │
                       ▼
                      Fact
              ┌────────┼────────┐
              │        │        │
           Evidence Confidence Status
              │        │        │
              └────────┼────────┘
                       │
                 StateEngine
                 /          \
                /            \
       aggregate()      aggregate_intervals()
           │                   │
           ▼                   ▼
     Current State       State Intervals
                               │
                               ▼
                       State Transition
                               │
                               ▼
                      Cognitive Context
                               │
                         ┌─────┴─────┐
                         │           │
                       Agent      Decision
                                     │
                              ┌──────┴──────┐
                              │             │
                           Because       Outcome
                              │             │
                              ▼             ▼
                            Facts        Future
```

注意：这里没有 Graph / Ontology / Reasoner / Causal Engine / Event Store / LLM Judge。

---

## 8. 里程碑

- **v0.3.0**：Step 1 + Step 2 合入（Fact 升维 + 状态区间），README 定位升级为 "Cognitive State Engine for Companion AI"
- **v0.3.1**：Step 3 合入（决策闭环，实验性；不触碰 memory_decay 语义）

---

## 9. 评审记录

### v1 → v2（2026-08-15）

| # | 评审意见 | 修订结果 |
|---|---|---|
| 1 | `derived_from` 不要与 causal 等同 | §3.2 明确为 Derivation/Provenance Chain；causal 未来单独引入 |
| 2 | `StateInterval` 与 `StateTransition` 概念分开 | §3.3 拆为两个独立结构体 |
| 3 | `TransitionType` 只保留三个 | §3.4 砍掉 TurningPoint |
| 4 | Decision 直接独立表，不要二选一摇摆 | §4 Step 3 拍板独立 `decisions` 表 |
| 5 | `because` = supporting evidence，不是 causality | §4 Step 3 明确 |
| 6 | transition 检测允许"不确定" | §4 Step 2 验收改为"不强制分类" |
| 7 | status / confidence / decay 三者严格正交 | §3.1 正交性原则 |
| 8 | 旧列 `weight`/`archived` 不要删太早 | §4 Step 1 双读迁移，major version 清理 |
| 9 | `outcome` 先允许为空 | §4 Step 3 `outcome: Option<...>` |
| 10 | 不创建 `CognitiveGraph` 抽象 | §6 scope 红线 |
| 11 | 验收不要算法绑定 | §4 Step 2 验收改为"保留三状态+evidence，transition 不强求" |
| 12 | Decision 降为 v0.3.1 实验性 | §1.3 版本定义 |

### v2 → v3（2026-08-15，冻结定稿）

| # | 评审意见 | 修订结果 |
|---|---|---|
| 1 | 明确 `Fact.time`（observation）与 `StateInterval.from/to`（validity）时间语义 | §2.2 写死语义；无可靠 valid time 时退化为 observation-derived interval；不新增字段 |
| 2 | `StateTransition` 不复制 Interval，存下标/引用 | §3.3 `from_index`/`to_index`，不引入持久化表 |
| 3 | `state_intervals` 保持泛型，防 god object | §4 Step 2 + §6 冻结为 `Vec<StateInterval>`，禁止 per-dimension 结构体 |
| 4 | **删掉 memory_decay 联动** | §4 Step 3：v0.3.1 不修改 decay 语义，是否纳入留待数据决定 |
| 5 | 补充核心 invariant：State 可重算、原始 Fact 永不消失 | §2.1（这是无需 Event Sourcing 的原因） |
