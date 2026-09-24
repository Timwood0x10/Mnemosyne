# 编译产出质量基线（compile yield）

> 度量对象：`memory_compile` 的用户通道 —— 一句真实口语能编译出什么事实。
> 为什么需要它：本项目承诺"陪伴型 AI 记住你"，但在 2026-09-23 之前这个承诺**从未被量化**：
> fixture 只有几百字节，没有任何测试衡量过真实口语的产出质量。前三轮评审发现的每个严重缺陷
> （变迁类型不可达、证据表从未写入、否定丢失）根因都是同一句：**代码从未在真实数据上跑过**。

## 1. 怎么跑

```bash
cargo test --test compile_yield -- --nocapture
```

报告通过 `tracing` 输出（`must_catch` / `should_catch` recall、幻影行数、过度抽取条数，以及每条
未命中的原句）。测试同时锁住不变量与回归下限，**掉了就红**：

- 每个事实必须携带原句（`payload.content`）与可回溯证据锚点（`payload.evidence`）；
- 同一句话编译两次必须字节级一致；
- 寒暄/噪声行产出任何事实都算幻影（当前 0/9）；
- 否定句不得存成肯定事实；
- `must_catch` recall 不得低于基线（见 §3）。

语料在 `tests/fixtures/compile_yield_zh.json`，加行即扩面，不用改代码。

## 2. 标注口径（三层）

| tier | 含义 | 用途 |
|---|---|---|
| `must_catch` | 当前机制（observation marker 表）**明确设计要命中**的表达 | 回归下限 |
| `should_catch` | 人类期望被记住、但当前机制可能覆盖不到的表达 | 度量能力缺口 |
| `must_ignore` | 寒暄与噪声 | 幻影率 |

## 3. 基线（2026-09-23，48 行语料 / 8 个会话 + 7 条承诺用例）

| 指标 | 值 |
|---|---|
| `must_catch` recall | **30/30 = 100%** |
| `should_catch` recall | **2/9 = 22%** |
| 幻影（噪声行产出事实） | **0/9** |
| 过度抽取（类型不在标注内） | 3 条 |
| 承诺抽取（`src/commitment.rs`） | 7/7 完全符合标注（含 3 条**必须不产出**的否定承诺） |

## 4. 能力缺口（`should_catch` 漏掉的 7 条，按性质分三类）

1. **身份 / 关系 / 兴趣类事实根本无法由 marker 表产出**（"我叫小林，在杭州做后端开发"、
   "我养了一只猫，叫豆豆"、"平时会去爬山"）。
   marker 表只覆盖九族动作：`喜欢` / `dislike` / `准备` / `plan` / `want` / `belief` / `stuck` /
   `life_event` / `feel`，映射到的类型只有 preference / goal / emotion / event。
   identity / relationship / interest / location / occupation / habit **不在其中**。
   （身份类事实目前只在 companion 信号通道里由 `FactType::Identity` 产出。）
2. **间接事件**："我妈下周要做手术"、"房东突然说要涨租" —— 没有显式情绪/偏好词，
   需要"人物 + 动作 + 时间"的事件模板，属于新抽取能力。
3. **口语化 want 短语不在表内**（"想出门"、"只想躺着"）。

## 5. 过度抽取的 3 条（已核查，非 bug）

| 事实 | 原因 | 处置 |
|---|---|---|
| `I'm so tired today…` → event | "think" 命中 `belief`，而 `belief` 经 `verb_to_fact_type` 落到 Event | 记录：模型没有 belief 维度，映射到 Event 只是"最不坏"的选择 |
| `我不喜欢应酬…` → emotion | "应酬" 同时在 `feel` 词表里 | 一条话产出 preference + emotion 两条，可接受 |
| `现在真的受不了加班文化了` → event | "受不了" 命中 dislike，另有 belief 路径 | 同上 |

## 6. 结论与下一步

- marker 表对**显式信号已经 100%** 覆盖：继续加词收益递减，且会推高幻影风险。
- 真正缺的是**通道**而不是词表：身份/关系/兴趣抽取、间接事件模板。
  这两件事都需要先决定模型位置（是否新增维度 / 复用 `FactType`），因此**先记录、不擅自加**。
- 任何改动 marker 表或抽取规则的 PR，跑一次本度量即可知道"有没有把显性信号弄丢"。
