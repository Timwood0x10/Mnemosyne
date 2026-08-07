# 陪伴型 AI MCP · 人设不崩改善计划

> 目标：把当前记忆后端升级为"陪伴型 AI 的 MCP + 维持人设不崩的工具"。
> 现状：记忆引擎（写/读/检索/蒸馏）约 80% 完成；"陪伴 + 人设不崩" 这一层约 20%。
> 缺口本质：缺三个出站闭环——**每轮注入人设卡** × **回复前校验人设冲突** × **关系/情绪状态持久化 + 记忆衰减**，并把已经写好但被灰度关掉的陪伴信号打开。
>
> **硬约束（用户明确）**：
> - 这是一个**确定性 MCP 后端**，拒绝使用 LLM。人格识别/校验一律用 **embedding 语义相似度**（纯向量算法），不引入任何模型生成。
> - embedding 不可用（无后端 / 网络失败）时，**自动回退关键词方案**，保证离线可用。
> - 语料是**通用混合体**：大量小说 + 真实陪伴对话。人格信号必须能按"说话人"归属到各自 entity（小说每角色一个 entity，真实对话里 agent / user 各一），不能只固定归给 agent。
> - **记忆调和走 mem0 v3 ADD-only 累积策略**：只新增、不覆盖、不删除，靠时间上下文 + 混合检索在取回时排序。核心诉求是**完整记录一个人的变化过程**——像解析三国演义里某人的生平一样，能随时重建"这个人从起点到现在的完整轨迹"。"陪伴的意义"就在于此。

---

## 一、现状盘点

### 已具备
| 能力 | 位置 |
|---|---|
| MCP 协议完整（stdio / HTTP / SSE，JSON-RPC 2.0，~25 工具） | `src/mcp/mod.rs` |
| 记忆蒸馏七件套（distill / compile / search / store / feedback / stats / context_check） | `src/main.rs` |
| 认知三通道（user / agent / derived），零污染保障 | `src/agent_facts.rs` |
| agent 人格通道：`我是/我喜欢/我不要` → Identity/Emotion/Preference/Goal，带正反标记 | `src/agent_personality.rs` |
| embedding 服务抽象（`EmbeddingService`，含 `enabled()` 标记 + `NullEmbedder` 兜底） | `src/embed.rs` |
| 向量相似度计算（HNSW / 暴力） | `src/vector/` |
| 陪伴信号提取：情绪序列 / 自我认知 / 重复主题 | `src/knowledge/companion_extract.rs` |
| 知识图谱 / 检索 / 记忆导出导入 / 反馈闭环 | `src/mcp/knowledge_tools.rs` |

### 关键缺口
1. **无"人设注入"出站工具** —— 缺失，最致命。
2. **无"人设不崩"的主动校验/冲突检测** —— 缺失，核心。
3. **人格识别是硬编码中文关键词 + `contains` 匹配** —— 需升级为 **embedding 语义原型向量**，并自动回退关键词。
4. **缺记忆调和决策（ADDN）** —— 现只有"提取即存"，没有对照已有记忆做 ADD/NOOP/UPDATE 的收敛，长期会累积重复与矛盾。
5. **`companion_extract` 处于灰度未接入生产** —— 需打开并验证。
6. **缺关系/亲密度/情感状态的持久状态机** —— 缺失。
7. **缺记忆衰减/遗忘管理** —— 缺失。

---

## 二、目标架构

```
        ┌─────────────────────────────────────────────────────────┐
        │  LLM Host（外部，负责对话生成）                            │
        └───────┬───────────────▲───────────────┬────────────────┘
                │ ① 每轮生成前     │ ② 回复前校验   │ ③ 每轮回写
                ▼               │               ▼
   ┌────────────────┐   ┌────────────────┐   ┌────────────────┐
   │ persona_inject │   │  persona_check │   │  persona_store │
   │  注入人设卡     │   │  冲突守卫       │   │  回写新事实     │
   └───────┬────────┘   └───────┬────────┘   └───────┬────────┘
           │                    │                    │
           ▼                    ▼                    ▼
   ┌──────────────────────────────────────────────────────┐
   │              Persona / Relationship 状态层            │
   │  人设卡 · 已存人设事实 · 关系状态 · 情感趋势 · 记忆衰减   │
   └──────────────────────────────────────────────────────┘
```

- **① 注入**（出站）：生成前把稳定人设卡打包给 host。
- **② 校验**（出站）：草稿回复 vs 已存人设事实，返回冲突/偏离警告。
- **③ 回写**（入站）：把新对话蒸馏成人设事实，更新关系状态。

---

## 三、核心底座：语义人格信号引擎（`PersonaSignalExtractor`）

> 这是"人设不崩"的感知层，替换掉硬编码关键词。纯 embedding 算法 + 关键词回退，无 LLM。

### 设计
- **原型句库**（可配置 JSON，替代硬编码 `PERSONALITY_MARKERS`）：
  - 每个 `(fact_type, negated)` 组合对应若干条**双语原型句**（身份/偏好/情绪/目标，含否定句）。
  - 启动时用 `EmbeddingService::embed_batch` 一次性嵌入并缓存为原型向量。
- **提取流程**：对每个说话人的发言 → `embed` → 与原型向量算余弦相似度（复用 `src/vector/`）→ **argmax 且超过阈值** → 判定为 `(fact_type, negated)`，归属到该说话人对应的 entity。
- **否定判别**：否定句作为**独立原型向量**编码，靠 argmax 区分，不靠向量方向推理。
- **说话人归属**：先通过现有 compiler 的说话人解析定位"谁在说"，再把第一人称信号写到对应 entity（小说角色 / agent / user），实现通用语料。
- **自动回退**：抽象成 trait，`EmbeddingPersonaExtractor`（语义）+ `KeywordPersonaExtractor`（现有关键词，兜底）。当 `EmbeddingService::enabled() == false` 或远程调用失败时，切换回关键词，保证离线可用。
- **阈值校准**：语义相似度阈值用混合语料（小说 + 真实对话）回归校准，避免误判。

### 记忆调和决策引擎（mem0 ADDN，无 LLM 版）
> 复刻 mem0 `add()` 的"调和"环节，但用 embedding 相似度 + 规则替代 LLM 推理。策略为 **v3 ADD-only 累积**。

对照已存事实，对每条候选事实做相似度比对，得到决策：
- **ADD**：与所有已有事实相似度 < 去重阈值 → 新事实，入库。
- **NOOP**：相似度 ≥ 去重阈值（如 0.9）→ 已存在，跳过（省存储）。
- **UPDATE 语义**：不物理覆盖，而是**保留两者 + 标记时间上下文**（"以前住纽约""现在住旧金山"并存），靠混合检索取回时排序。
- **冲突/转变**：`negated` 相反 + 高相似 → 标记为"转变点"，**不删除**，供 `persona_check` 与时间线展示。

### 关键任务
- [x] P1. 定义 `PersonaSignalExtractor` trait + 原型句库加载（JSON 可配置）。
- [x] P2. 实现 `EmbeddingPersonaExtractor`：原型向量缓存 + 余弦相似度 + argmax 阈值判定。
- [x] P3. 实现 `KeywordPersonaExtractor`（平移现有关键词逻辑，作为回退）。
- [x] P4. 接入说话人归属：按发言者把信号写到对应 entity。
- [x] P5. 阈值校准 + 混合语料回归（复用 `tests/` 中小说/对话用例）。
- [x] P6. 实现记忆调和决策器：候选事实 vs 已存事实 → ADD / NOOP / 冲突标记（v3 累积）。
- [x] P7. 调和器单元测试：重复识别、正反转变标记、时间上下文保留。

---

## 四、分阶段任务

### 阶段 A：人设冲突守卫（`persona_check`）—— 最贴近"不崩"，优先做

> 复用语义人格引擎，做"存"之后的"比对"。

- [x] A1. 定义 `persona_facts` 读取接口：按 entity 拉取全部 `agent_personality` 标记的事实（含说话人归属）。
- [x] A2. 实现 `persona_check` MCP 工具 handler：
  - 输入：`draft_reply`（agent 草稿）+ `tenant_id` / `user_id`。
  - 逻辑：用语义引擎对草稿提取人格信号，与已存人设事实比对。
  - 冲突判定：同一实体、同一 `fact_type`、正反标记（`negated`）相反、且语义相似度在阈值内 → 冲突。
  - 输出：`{ conflicts: [...], drift: [...] }`，给出证据引用。
- [x] A3. 注册工具 + 输入 schema + 单元测试（含正反标记冲突用例）。
- [x] A4. 接入 host 契约文档：在生成前调用，冲突时提示 host 改稿或降级。

### 阶段 B：人设注入（`persona_inject`）—— 陪伴型入口

- [x] B1. 设计**人设卡数据结构**（结构化稳定约束，替代散落关键词）：
  - `identity`（我是谁）、`persona`（性格/口吻）、`style`（言说风格）、`taboos`（禁忌/绝对不说的）、`relationship`（与用户的关系状态）。
- [x] B2. 人设卡来源：a) 导入 JSON 人设卡文件；b) 从 `agent_personality` 事实自动聚合生成。
- [x] B3. 实现 `persona_inject` 工具：返回可拼进 system prompt 的文本/JSON 人设包。
- [x] B4. 支持按 `tenant_id` 多角色多份人设卡切换（适配小说多角色 + 真实对话多 agent）。

### 阶段 C：打开陪伴信号 + 关系状态

- [x] C1. 将 `COMPANION_EXTRACT_GRAYSCALE` 置为 `false`，接入 `agent_fact_compile` 编译链路。
- [x] C2. 用混合语料（小说 + 真实对话）回归验证 `emotion_series` / `self_cognition` / `repeated_themes` 的提取质量。
- [x] C3. 设计**关系状态表**：`relationship_state`（亲密度、关系阶段、最近共同话题、情感趋势）。
- [x] C4. 实现 `relationship_update`：每轮对话后根据情绪信号增量更新关系状态。
- [x] C5. 实现 `relationship_query`：跨会话读取当前关系/情感状态快照。
- [x] C6. **实现"人设演进时间线"**：把累积的 `fact`（含时间上下文）按时间组织成"一个人的完整变化过程"，复用现有 `timeline` / `key_events` / `story_events` 能力，像解析三国演义某人生平一样，可输出「起点 → 关键转变点 → 现状」的完整轨迹。

### 阶段 D：记忆衰减/遗忘管理

- [x] D1. 设计衰减策略配置（`decay` 参数：按时间/按重要性/按访问频率）。
- [x] D2. 实现后台衰减任务：对低重要度 / 久未访问的 persona 记忆降权或归档。
- [x] D3. 提供 `memory_decay` 触发入口 + 手动可调，防止长期陪伴记忆膨胀。
- [x] D4. **衰减不破坏演进证据**：降权/归档只影响检索排序，不删除历史 `fact`，确保"人设演进时间线"仍可完整重建。

### 阶段 E：工程化收尾

- [x] E1. 更新 `README` / `README.zh.md` 工具清单与 host 集成契约。
- [x] E2. 补充端到端测试：注入 → 生成 → 校验 → 回写 → 状态更新全链路。
- [x] E3. 代码评审 + 性能/存储回归。

---

## 五、优先级与依赖

| 任务 | 依赖 | 优先级 | 理由 |
|---|---|---|---|
| 语义人格引擎（PersonaSignalExtractor） | 现有 `embed.rs` + `vector/` | P0 | 一切人格感知的底座 |
| A 人设冲突守卫 | 语义人格引擎 | P0 | 最贴近"不崩"，最易落地 |
| B 人设注入 | 无 | P0 | 陪伴型入口，与 A 互补 |
| C 陪伴信号 + 关系状态 | A | P1 | 需先打开灰度再建状态 |
| D 记忆衰减 | 无 | P1 | 长期陪伴的刚需 |
| E 工程化收尾 | A/B/C/D | P2 | 收口 |

> 建议落地顺序：**底座 → A → B → C → D → E**。底座和 A 可单独交付，先拿价值。

---

## 六、验收标准

- [x] `PersonaSignalExtractor` 在 embedding 可用时用语义匹配，不可用时自动回退关键词，行为一致。
  → `src/persona/embedding_extractor.rs` + `keyword_extractor.rs`；`check.rs::extract_signals` 按 `cache.is_some()` 分流。
- [x] 语义引擎能正确识别正反标记冲突（如"我不喜欢" vs 草稿"我喜欢"）并给出证据。
  → `tests/mcp_corpus_real_dialog.rs` 实测：白流苏语料矛盾草稿 → conflicts=1（含 stored_fact_id/stored_content/similarity 证据）。
- [x] 语义引擎在**小说 + 真实对话**混合语料上都能按说话人归属到正确 entity。
  → `tests/mcp_bailiusu.rs`/`mcp_warpeace.rs`/`mcp_corpus_real_dialog.rs`：agent vs user 各自 entity 落库（白流苏 agent=57/user=18，皮埃尔 agent=81/user=14）。
- [x] `persona_inject` 能返回完整人设包，支持多角色切换。
  → `src/mcp/persona_inject_tool.rs` + `tests/companion_persona_e2e.rs` 阶段 2；按 `tenant_id`/`agent_id` 隔离多角色。
- [x] `companion_extract` 灰度打开后，情绪/自我认知/重复主题可落库并可查询。
  → `src/knowledge/companion_extract.rs`（`COMPANION_EXTRACT_GRAYSCALE`）接入 `agent_fact_compile`；实测 conversation_export 语料落 28 条人格事实。
- [x] `relationship_state` 能跨会话反映亲密度与情感趋势变化。
  → `src/relationship.rs` + `src/mcp/relationship_tool.rs`；`tests/companion_persona_e2e.rs` 阶段 4-5：update→query 亲密度一致。
- [x] 记忆调和器能识别重复（NOOP）、标记正反转变点，且不覆盖已有人设事实（v3 累积）。
  → `src/persona/reconciler.rs`（`ReconcileDecision::Add/Noop/AddWithTransition`）；`tests/persona/integration_tests.rs` 6 例端到端。
- [x] **人设演进时间线**能输出某人的完整变化过程（起点 → 关键转变点 → 现状），在小说与真实对话语料上都成立。
  → `src/persona/timeline.rs` + `tests/mcp_story_bridge.rs`：流苏 179 事件 → timeline 起点→181 转折→现状；`companion_persona_e2e.rs` 阶段 6 stance_flip 里程碑。
- [x] 记忆衰减任务可配置并可手动触发，不丢高价值人设事实，且不破坏演进时间线。
  → `src/decay.rs` + `src/mcp/decay_tool.rs`；`companion_persona_e2e.rs` 阶段 7：scanned=3, high_value_protected=3（persona 事实全保）。
- [x] 全链路端到端测试通过，`cargo test` 全绿。
  → `cargo test --lib`：645 passed / 0 failed；6 套 persona 集成测试（`mcp_corpus_full_loop` / `mcp_corpus_real_dialog` / `companion_persona_e2e` / `mcp_bailiusu` / `mcp_warpeace` / `mcp_story_bridge`）全绿，覆盖 4 语料（白流苏/皮埃尔/真实编码对话/索尼娅×拉斯柯尔尼科夫）。