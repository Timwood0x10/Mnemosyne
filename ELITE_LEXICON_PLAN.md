# Elite Lexicon 计划

> 面向 Human Cognition Compiler 的中英文高信息密度词库。
>
> 本计划中的“精英词”不是常用词大全，也不是传统词典的缩小版。精英词是能够稳定改变 `Observation`、`Fact`、`Relation`、`State` 或实体识别结果，并且能够被回归测试证明有价值的词条。

---

## 1. 决策摘要

项目需要维护自己的 **Elite Lexicon（精英词库）**，但不直接内置《牛津词典》《新华字典》等商业词典内容，也不追求收录所有常用名词和动词。

精英词库应当成为 `Language Frontend` 的结构化参考资料，并遵循以下原则：

1. **少而强**：仅收录会改变认知编译结果的高价值词。
2. **语义优先**：词条必须携带动作类别、关系影响、事实类型等机器可执行语义。
3. **证据驱动**：没有语料证据和回归用例的词不得进入核心层。
4. **中英文对齐**：不同语言共享统一语义类别，但保留各自词形和语法特征。
5. **核心可裁剪**：默认内置词表必须小，不显著增加二进制体积和启动成本。
6. **索引只构建一次**：每次编译复用预构建的 Aho-Corasick 和 HashMap 索引。
7. **许可可追溯**：每条外部来源数据必须记录来源、版本和许可证。
8. **输出必须确定**：词表顺序、冲突处理和编译结果不得依赖 HashMap 随机顺序。
9. **用户可覆盖**：用户词条可以增强或覆盖内置词条，但不能静默改变系统行为。
10. **商业数据外置**：牛津、新华等商业词典仅允许通过用户自备授权的适配器接入。

---

## 2. 为什么需要精英词库

当前语言规则分散在多个位置：

- `src/language.rs`：中英文动词、关系词和 Profile Pattern；
- `src/observation_compiler.rs`：FactType 动词分类和对话词；
- `src/compiler/extract.rs`：事件动词匹配；
- `src/compiler/profile.rs`：人物画像模式和默认模式；
- `src/compiler/timeline.rs`：关系变化与性格模式；
- `src/ingest/extract.rs`：语料摄取动作词；
- `src/ingest/relation.rs`：对话和关系触发词；
- `config/entity_profiles/*.json`：作品级动作、关系和人物配置。

这种分散方式存在以下问题：

1. 同一个词可能在多个模块重复定义，语义却不完全相同；
2. 新增一个词时容易只修复一条路径；
3. 中英文语义类别无法稳定对齐；
4. 匹配器生命周期不统一，出现每句、每章重复构建；
5. 难以回答“这个词为什么存在、影响什么、由哪个测试保护”；
6. 无法统计词条的命中率、误报率和实际贡献；
7. 通用词典数据一旦直接混入核心规则，很难控制体积、歧义和许可证。

精英词库的目标是把这些规则收敛成一个可治理、可测试、可度量的数据层。

---

## 3. “精英词”的严格定义

一个词条只有满足以下定义，才可以进入精英词库：

> 在明确语言和上下文条件下，该词条能够以可测试、可重复的方式改变认知编译产物，并且其收益高于由歧义、误报、体积和维护成本产生的代价。

### 3.1 准入条件

核心词条必须同时满足以下条件：

| 条件 | 要求 |
|---|---|
| 语义作用 | 能映射到 Action、FactType、RelationType、EntityType、Modifier 或状态变化 |
| 语料证据 | 至少在一套固定回归语料中出现，或由真实会话案例证明 |
| 可测试性 | 至少包含一个正例和一个反例 |
| 歧义控制 | 已定义语言、词性、上下文要求或优先级 |
| 贡献明确 | 删除该词会导致可观察的召回下降或语义退化 |
| 来源明确 | 自主整理、开放数据或用户自定义，来源可追踪 |
| 运行成本 | 不要求默认加载大型外部数据库或网络服务 |

### 3.2 进入核心层的附加条件

进入默认内置核心层还必须满足以下至少两项：

- 同时覆盖小说、对话、文档等多种输入；
- 在至少两套固定语料中命中；
- 对高价值事实类型有直接贡献；
- 命中频率较高且误报率可控；
- 属于否定、时态、意图、情绪等基础语言能力；
- 是其他领域词包共同依赖的基础语义。

### 3.3 不属于精英词的内容

以下内容默认不得进入核心层：

- 只用于展示释义、音标或例句的普通词条；
- 没有语义类别的完整常用词列表；
- 从商业词典复制的释义、例句或词条编排；
- 仅在单一作品出现的人名、地名和武器名；
- 只提高匹配数量但无法提高事实质量的弱触发词；
- 高歧义且没有上下文约束的单字词；
- 无固定测试、无来源、无负责人说明的临时词。

---

## 4. 词库分层

### 4.1 Core Elite Lexicon

默认内置，体积最小，随程序发布。

建议首版目标：

- 中文 300～800 个标准词形；
- 英文 300～800 个 lemma；
- 加上变体后总 pattern 数不超过 3000；
- 序列化原始数据建议控制在 500 KiB 以内；
- 构建索引后的常驻内存目标不超过 5 MiB。

核心类别：

- action；
- relation；
- speech；
- cognition；
- intention；
- preference；
- emotion；
- state；
- location；
- possession；
- identity；
- time；
- negation；
- uncertainty；
- title；
- entity-kind。

### 4.2 Domain Packs

按领域或作品加载，不进入默认核心：

```text
classical_chinese
modern_chinese
english_narrative
conversation_memory
software_engineering
finance
medical
sanguo
fengshen
honglou
xiyou
```

领域包负责处理：

- 行业术语；
- 古汉语特殊表达；
- 作品专属人物称谓；
- 专属关系和事件触发词；
- 项目或租户自己的概念体系。

### 4.3 User Overrides

用户自定义层具有最高配置优先级，但必须显式加载。

支持：

- 新增词条；
- 禁用内置词条；
- 修改优先级；
- 收紧上下文条件；
- 增加别名和词形；
- 将未知词提升到领域词包。

不得允许用户数据静默覆盖语义而没有来源记录。

### 4.4 External Reference Adapters

只提供接口，不默认携带数据：

- Open English WordNet；
- Princeton WordNet；
- 合规的中文开放词库；
- 用户本地词典文件；
- 用户持有授权的牛津 API；
- 用户持有授权的中文商业词典。

外部通用词典用于辅助词性、lemma、同义关系和候选生成，不直接决定认知事实。

---

## 5. 首版语义分类

### 5.1 动作词

动作词是首版最高优先级。

| 类别 | 中文示例 | 英文示例 | 默认影响 |
|---|---|---|---|
| attack | 杀、攻击、刺杀 | kill, attack, murder | Event + hostile relation |
| rescue | 救、营救、援助 | save, rescue, aid | Event + friendly relation |
| movement | 去、来到、逃往 | go, arrive, flee | Event + Location |
| creation | 创建、建立、生成 | create, build, generate | Event / Identity |
| destruction | 删除、摧毁、废除 | delete, destroy, abolish | Event / State |
| transfer | 给、授予、转让 | give, grant, transfer | Event + possession |
| speech | 说、询问、回答 | say, ask, reply | Dialogue event |
| cognition | 知道、认为、发现 | know, think, discover | Belief / Knowledge |
| intention | 想、计划、决定 | want, plan, decide | Goal |
| preference | 喜欢、偏好、厌恶 | like, prefer, dislike | Preference |
| emotion | 害怕、愤怒、悲伤 | fear, anger, grieve | Emotion |
| state | 成为、拥有、居住 | become, own, live | Identity / State |

### 5.2 名词和实体触发词

不维护完整名词词典，只维护能提高实体分类的精英名词：

| 类别 | 中文示例 | 英文示例 |
|---|---|---|
| person-title | 先生、夫人、将军、教授 | Mr., Mrs., General, Professor |
| organization-kind | 公司、大学、委员会 | company, university, committee |
| location-kind | 城市、国家、办公室 | city, country, office |
| artifact-kind | 武器、车辆、软件 | weapon, vehicle, software |
| occupation-kind | 工程师、医生、教师 | engineer, doctor, teacher |
| time-unit | 年、月、小时 | year, month, hour |
| relationship-kind | 父亲、朋友、同事 | father, friend, colleague |

具体名称由 Alias Registry、Entity Resolver 和领域包负责，不进入通用核心层。

### 5.3 功能词和修饰词

这些词数量少，但对事实正确性极重要：

- 否定：不、没、未、无、not、never、no longer；
- 不确定：可能、也许、据说、may、might、probably；
- 时间：曾经、已经、正在、将要、was、is、will；
- 强度：非常、略微、极其、very、slightly、extremely；
- 条件：如果、除非、if、unless；
- 事实来源：我认为、他说、据报道、I think、he said、reportedly。

这部分应优先于扩充普通名词，因为它直接影响事实置信度、有效期和归属。

---

## 6. 词条模型

建议定义强类型模型，而不是直接维护多个 `Vec<String>`：

```rust
pub struct Lexeme {
    pub id: LexemeId,
    pub language: LanguageCode,
    pub lemma: String,
    pub forms: Vec<String>,
    pub part_of_speech: PartOfSpeech,
    pub semantic_class: SemanticClass,
    pub cognitive_effects: Vec<CognitiveEffect>,
    pub polarity: Polarity,
    pub priority: u16,
    pub constraints: MatchConstraints,
    pub source: LexiconSource,
    pub evidence: Vec<LexiconEvidence>,
    pub status: LexemeStatus,
}
```

关键枚举建议包括：

```rust
pub enum PartOfSpeech {
    Verb,
    Noun,
    Adjective,
    Adverb,
    Particle,
    Phrase,
}

pub enum LexemeStatus {
    Candidate,
    Experimental,
    Core,
    Deprecated,
    Disabled,
}

pub enum CognitiveEffect {
    Fact(FactType),
    Relation(RelationType),
    Entity(EntityType),
    Event(EventType),
    Modifier(ModifierType),
    ConfidenceDelta(i16),
}
```

`MatchConstraints` 至少应支持：

- 大小写策略；
- 英文单词边界；
- 中文单字是否允许独立匹配；
- 前后允许或禁止的词类；
- 是否要求主语或宾语；
- 是否要求否定检测；
- 支持的输入域；
- 最短上下文长度；
- 冲突词条优先级。

---

## 7. 推荐数据格式

词条原始资料建议使用可审查的 JSON 文件。运行时加载后转换为强类型结构。

示例：

```json
{
  "id": "zh.action.attack.kill",
  "language": "zh",
  "lemma": "杀",
  "forms": ["杀死", "杀害", "斩杀"],
  "part_of_speech": "verb",
  "semantic_class": "attack",
  "cognitive_effects": [
    { "type": "fact", "value": "event" },
    { "type": "relation", "value": "hostile" }
  ],
  "polarity": "negative",
  "priority": 900,
  "constraints": {
    "allow_single_character": true,
    "requires_participant": true
  },
  "source": {
    "kind": "builtin",
    "name": "LoreScope Elite Lexicon",
    "license": "Apache-2.0"
  },
  "status": "core"
}
```

英文词条：

```json
{
  "id": "en.intention.plan",
  "language": "en",
  "lemma": "plan",
  "forms": ["plans", "planned", "planning"],
  "part_of_speech": "verb",
  "semantic_class": "intention",
  "cognitive_effects": [
    { "type": "fact", "value": "goal" }
  ],
  "polarity": "neutral",
  "priority": 800,
  "constraints": {
    "word_boundary": true,
    "requires_subject": true
  },
  "source": {
    "kind": "builtin",
    "name": "LoreScope Elite Lexicon",
    "license": "Apache-2.0"
  },
  "status": "core"
}
```

---

## 8. 建议项目结构

实施时建议创建独立模块，避免继续扩大 `src/language.rs`：

```text
src/lexicon/
  mod.rs
  model.rs
  registry.rs
  matcher.rs
  loader.rs
  validation.rs
  metrics.rs

lexicon/
  core/
    zh.json
    en.json
  packs/
    classical_chinese.json
    conversation_memory.json
    english_narrative.json
  licenses/
    README.md
  schema/
    lexeme.schema.json

tests/
  lexicon_core.rs
  lexicon_regression.rs
```

约束：

- 每个 Rust 文件不超过 1000 行；
- 每种职责单独模块化；
- 生产代码不得使用 `unwrap()` 处理词库输入；
- 代码注释使用英文；
- JSON 数据按稳定键和稳定词条 ID 排序；
- 核心词库不得依赖网络；
- 外部词典包不得混入核心 JSON。

---

## 9. 运行时架构

```text
Core lexicon ─────────┐
Domain packs ─────────┤
User overrides ───────┼──> Lexicon Registry
External adapters ────┘          │
                                 ├──> Alias matcher
                                 ├──> Verb matcher
                                 ├──> Modifier matcher
                                 ├──> Title/entity matcher
                                 └──> Semantic lookup
```

### 9.1 Lexicon Registry

职责：

- 合并多层词表；
- 校验 ID、词形和许可证元数据；
- 执行覆盖和禁用规则；
- 解决词条优先级冲突；
- 生成稳定排序后的运行时快照；
- 为每个 matcher 提供只读数据。

建议优先级：

```text
user override > domain pack > core > external reference
```

外部参考词典默认只补充 lemma/POS/同义信息，不覆盖核心认知效果。

### 9.2 Matcher 生命周期

所有 matcher 应在 Registry 构建后创建一次：

- 中文和英文分开构建；
- 词类和语义类别可共享一个总 AC，再通过 pattern ID 查元数据；
- Alias matcher 保持最长优先和确定性 tie-break；
- 英文启用单词边界检查，避免 `plan` 命中 `planet`；
- 中文单字词必须显式允许，避免高误报；
- 编译循环中不得重建 verb 或 alias AC。

### 9.3 二进制体积策略

首版建议采用以下顺序：

1. Core JSON 通过 `include_bytes!` 内置；
2. 启动时只解析一次，并通过 `OnceLock` 或编译器实例持有；
3. Domain Pack 从外部文件按需加载；
4. 大型开放词库永不进入默认 feature；
5. 可选词库使用独立 Cargo feature 或运行时数据目录；
6. 每次发布记录 core 数据大小和最终二进制变化。

如果 JSON 解析成本成为已证实热点，再考虑构建时生成紧凑静态表，不提前引入代码生成复杂度。

---

## 10. 词条生命周期

### 10.1 候选产生

候选词来源：

- 全本回归中未识别但高频的动作词；
- 真实会话中遗漏的 Goal、Preference、Emotion 表达；
- 误分类问题的根因分析；
- Domain Pack 中跨领域重复出现的词；
- 合法开放词库产生的候选；
- 用户明确提交的领域词。

自动统计只能产生 Candidate，不能自动进入 Core。

### 10.2 晋升流程

```text
Candidate
  → 添加语义和约束
  → 添加正例、反例、歧义例
  → Experimental
  → 运行固定语料回归
  → 评估增量召回和增量误报
  → Core 或 Domain Pack
```

### 10.3 降级和淘汰

满足以下任一条件时进入审查：

- 连续多个版本没有命中；
- 误报率超过该类别门槛；
- 与更强词条完全重复；
- 只对单一作品有价值；
- 来源或许可证无法确认；
- 新规则已不再依赖该词条；
- 导致中英文输出不一致或非确定结果。

词条先标记 `Deprecated`，至少保留一个版本后再禁用。删除任何项目文件仍需遵守项目规则并单独获得授权。

---

## 11. 许可与来源治理

### 11.1 默认允许进入核心的数据

- 项目自行整理并以项目许可证发布的词条；
- 用户明确授权贡献的原创数据；
- 已确认兼容项目分发方式的开放数据；
- 单纯语言事实经独立整理后形成的原创语义标注。

### 11.2 默认禁止进入核心的数据

- 牛津词典的释义、例句、词条数据库和系统性导出；
- 新华字典的释义、例句、编排和系统性词条复制；
- 来源不明的网络词表；
- 许可证不允许再分发或要求无法满足的数据；
- ShareAlike 数据在没有隔离和合规审查前直接混入核心文件。

### 11.3 外部数据包要求

每个外部词库包必须携带：

```text
source name
source URL
source version/date
license identifier
copyright notice
attribution text
modification record
redistribution constraints
```

商业词典适配器不得默认缓存或导出完整数据。具体行为必须服从用户持有的授权条款。

---

## 12. 与现有代码的迁移策略

迁移必须保证功能不打折，禁止一次性删除旧常量。

### 阶段 A：建立基线，不改变行为

1. 固定当前中英文动词、关系词和 Profile Pattern 快照；
2. 记录三国、Pride and Prejudice、War and Peace 的输出 counts 和稳定 hash；
3. 为 Conversation Cognition 固定 Goal、Preference、Emotion、Identity 用例；
4. 增加阶段级计时，但不更改规则；
5. 记录当前二进制大小和各 matcher 构建次数。

### 阶段 B：建立模型和 Registry

1. 新增 `src/lexicon/model.rs`；
2. 新增 `LexiconRegistry`；
3. 支持 Core、Domain、User 三层合并；
4. 实现重复 ID、重复词形、非法优先级和缺失来源校验；
5. 保持现有 `LanguageProvider` API 可用。

### 阶段 C：导入现有规则

按模块逐个迁移：

1. `observation_compiler.rs` 的 FactType 动词分类；
2. `language.rs` 的 strong/action/hostile/friendly verbs；
3. 对话 marker；
4. timeline 关系和性格触发词；
5. ingest 动词和关系词；
6. entity title 和 entity-kind 词。

每迁移一个模块必须：

- 跑原模块测试；
- 对比迁移前输出；
- 确认没有召回下降；
- 确认错误分类没有增加；
- 再进入下一个模块。

### 阶段 D：统一运行时 matcher

1. Registry 生成 verb matcher；
2. Registry 生成 modifier matcher；
3. Registry 生成 title/entity matcher；
4. `extract.rs` 移除每句 verb AC 重建；
5. `ingest` 移除每章 alias AC 重建；
6. `profile.rs` 移除每行 alias 收集和排序；
7. 保证 matcher 只在 compile 或 compiler 初始化阶段构建一次。

此阶段只在新路径输出与旧路径等价后，才能停止使用旧常量。是否删除旧数据必须另行确认。

### 阶段 E：领域包与用户扩展

1. 加入 `classical_chinese`；
2. 加入 `conversation_memory`；
3. 加入 `english_narrative`；
4. 支持用户 JSON override；
5. 暴露有效词库快照和冲突诊断；
6. 支持按 tenant 或 compile request 选择领域包。

### 阶段 F：开放词库适配

1. 先实现离线 `ExternalFileProvider`；
2. 评估 Open English WordNet；
3. 评估中文开放词库；
4. 将外部词库限制为候选、lemma/POS 和同义辅助；
5. 未经显式配置不得影响核心认知输出。

---

## 13. 测试计划

### 13.1 模型与加载测试

必须覆盖：

- 正常加载中英文词条；
- 重复 ID 返回明确错误；
- 同层冲突返回明确错误；
- 无效语义类别返回明确错误；
- 缺失来源或许可证时拒绝外部包；
- 用户覆盖按确定优先级生效；
- 禁用词条不进入 matcher；
- 词条排序在多次运行中一致；
- 非法 UTF-8 或损坏 JSON 不得 panic。

### 13.2 匹配测试

中文至少覆盖：

- 长词优先；
- 单字歧义；
- 否定范围；
- 人名和动词相邻；
- 古汉语与现代汉语冲突；
- 同一个词具有多种语义时的约束选择。

英文至少覆盖：

- 单词边界；
- 大小写；
- 不规则过去式；
- 现在分词；
- phrasal verb；
- `plan` 不得命中 `planet`；
- `kill`、`killed`、`killing` 映射到同一 lemma。

### 13.3 认知语义测试

每类核心效果至少包含：

- 正例；
- 否定例；
- 不确定例；
- 引述归属例；
- 主语缺失例；
- 反事实或条件句例；
- 中英文语义对齐例。

示例：

```text
I plan to rewrite the parser.        → Goal
I do not plan to rewrite the parser. → 不得产生肯定 Goal
Alice said Bob plans to leave.       → Goal 归属 Bob，而不是 Alice
如果我计划离开……                    → 不得当作已经成立的 Goal
```

### 13.4 全本回归

固定语料：

- 三国演义；
- Pride and Prejudice；
- War and Peace；
- 小型中英文人工 fixture。

至少比较：

```text
entities count
profiles count
events count
relations count
observations by semantic class
facts by FactType
false-positive blacklist
stable output hash
```

词表迁移阶段要求功能不打折：迁移后关键 counts 不得无解释下降，稳定集合必须保持等价。

### 13.5 性能测试

在 release profile 下重复测试并记录 median/p95：

- Core JSON 加载时间；
- Registry 合并时间；
- AC 构建时间；
- Profile extraction；
- Mention scan；
- Event extraction；
- 全本总时间；
- 峰值内存；
- 二进制大小。

首版建议门槛：

| 指标 | 门槛 |
|---|---|
| Core 词库加载 | 本地 release 中位数小于 20 ms |
| Matcher 构建 | 本地 release 中位数小于 50 ms |
| 默认二进制增长 | 不超过 1 MiB，目标不超过 500 KiB |
| Core 原始数据 | 不超过 500 KiB |
| Matcher 构建次数 | 每个 compiler/compile 生命周期一次 |
| 输出确定性 | 同输入连续运行 hash 完全一致 |
| 全本性能 | 不得慢于迁移前，目标至少提升 20% |

性能数字需要在当前机器建立基线后校准，不作为未经测量的速度承诺。

---

## 14. 词条质量评分

候选词可以用评分辅助评审，但不能完全自动决定：

```text
elite_score =
    0.25 × semantic_impact
  + 0.20 × cross_domain_value
  + 0.20 × corpus_frequency
  + 0.15 × precision
  + 0.10 × test_coverage
  + 0.10 × maintenance_stability
```

每项取 0～1。

建议规则：

- `elite_score >= 0.80`：可申请进入 Core；
- `0.60 <= elite_score < 0.80`：保留在 Domain Pack；
- `0.40 <= elite_score < 0.60`：Experimental；
- `< 0.40`：不进入运行词库。

高频不等于高价值。否定词可能数量很少，却比数千个普通名词更重要。

---

## 15. 可观测性与治理

运行时建议记录聚合指标，不记录敏感原文：

- 每个词条命中次数；
- 每个语义类别命中次数；
- 因约束被拒绝的候选数；
- 冲突决策次数；
- 未知高频候选的匿名统计；
- 词条导致的 Fact 数；
- 词条导致的回滚或误报报告数；
- matcher 构建耗时；
- 当前词库版本和内容 hash。

不得把用户原始消息作为词表遥测直接持久化。候选提取应采用脱敏、hash 或用户明确授权的离线分析。

每个正式版本生成词库清单：

```text
version
content hash
core entry count
forms count
entries by language
entries by semantic class
added/deprecated/disabled entries
license inventory
regression summary
```

---

## 16. 明确非目标

本项目不以以下内容为目标：

- 制作完整中英词典；
- 替代牛津词典、新华字典或专业语言学数据库；
- 提供面向人的词语释义和例句查询；
- 默认内置数十万普通名词；
- 通过词表替代 Entity Resolver；
- 通过词表替代语法、否定、指代和事实归属判断；
- 自动把语料中的所有高频词加入核心；
- 为了召回率牺牲事实精度和可解释性；
- 未经许可抓取或再分发商业词典内容。

---

## 17. 实施任务清单

### P0：基线和规范

- [ ] 固定现有词表和规则来源清单；
- [ ] 定义 `Lexeme`、枚举和 JSON Schema；
- [ ] 确定稳定 ID 命名规则；
- [ ] 建立来源和许可证字段；
- [ ] 建立中英文人工 fixture；
- [ ] 记录全本 counts、hash、耗时和二进制大小；
- [ ] 先修复当前 `make test` 的 5 个 fixture/SQLite 失败，恢复可信全量基线。

### P1：Core Registry

- [ ] 实现 `LexiconRegistry`；
- [ ] 实现 Core/Domain/User 合并；
- [ ] 实现冲突、重复、来源校验；
- [ ] 实现稳定排序和内容 hash；
- [ ] 实现强类型加载错误；
- [ ] 添加模型、加载和覆盖测试。

### P2：迁移认知规则

- [ ] 迁移 Goal 动词；
- [ ] 迁移 Preference 动词；
- [ ] 迁移 Emotion 动词；
- [ ] 迁移 Identity/Occupation/Location 动词；
- [ ] 迁移 Event 和 hostile/friendly 动词；
- [ ] 迁移 speech markers；
- [ ] 加入否定、不确定、时间和引述词；
- [ ] 每类完成后执行模块测试并对比旧输出。

### P3：统一高性能 matcher

- [ ] 构建一次 verb AC；
- [ ] 构建一次 modifier AC；
- [ ] 构建一次 title/entity-kind AC；
- [ ] 预构建 Profile alias matcher；
- [ ] 预构建每部小说的 Ingestion alias matcher；
- [ ] 使用稳定 pattern ID 映射语义；
- [ ] 验证 matcher 生命周期和构建次数。

### P4：领域包

- [ ] `conversation_memory`；
- [ ] `classical_chinese`；
- [ ] `english_narrative`；
- [ ] 将作品专属词从 Core 隔离；
- [ ] 支持按 compile request 选择 pack；
- [ ] 增加 pack 冲突诊断。

### P5：开放参考词典

- [ ] 编写外部词典许可证审查表；
- [ ] 实现 `ExternalFileProvider`；
- [ ] 验证 WordNet 数据适配；
- [ ] 评估中文开放词库；
- [ ] 确保外部数据默认不决定 FactType；
- [ ] 为商业词典保留授权适配接口，但不内置数据。

### P6：治理和候选循环

- [ ] 实现命中和拒绝指标；
- [ ] 生成未知高频候选报告；
- [ ] 实现 Candidate/Experimental/Core 生命周期；
- [ ] 建立每个版本的词库清单；
- [ ] 建立弃用和回滚机制；
- [ ] 将词库 hash 写入编译诊断信息。

---

## 18. 每阶段质量门禁

每个模块完成后必须独立测试，然后才能继续下一模块。

代码阶段统一要求：

1. 不删除任何项目文件；
2. 不执行任何 Git 命令；
3. 不执行覆盖率测试；
4. 单个代码文件不超过 1000 行；
5. 生产代码注释使用英文；
6. 生产加载路径使用 `Result`，不得用 `unwrap()` 吞掉数据错误；
7. 每条核心规则包含正例、反例和歧义测试；
8. 所有断言包含明确错误消息；
9. 每次修改后执行 `make fmt`；
10. 执行模块测试；
11. `make check` 必须 0 errors；
12. 最终执行真实 `make test`，必须 0 failed；
13. 不得用 `#[allow(dead_code)]` 掩盖 warning；
14. 迁移前后功能和固定回归输出不得打折。

当前已知全量测试仍有 5 项 fixture/SQLite 隔离失败。词库开发开始前应优先修复这些失败，否则无法建立可信的全量回归基线。

---

## 19. 首版完成定义

首版只有同时满足以下条件才算完成：

- Core 中英文词库已经强类型加载；
- 词条具有稳定 ID、语义、来源、状态和约束；
- Goal、Preference、Emotion、Identity、Event、Relation 已使用 Registry；
- 旧规则迁移前后关键回归无未经解释的下降；
- 中文和英文 matcher 都只构建一次；
- 支持至少一个 Domain Pack；
- 支持用户 override 和禁用；
- 输出具有稳定 hash；
- 核心词库许可证清晰；
- 未内置牛津、新华等商业词典内容；
- Core 原始数据和二进制增长满足体积门槛；
- 三国、Pride and Prejudice、War and Peace 回归通过；
- `make check` 0 errors；
- `make test` 0 failed。

---

## 20. 最终原则

精英词库的价值不由词条数量决定，而由以下问题决定：

> 如果删除这个词条，认知编译器是否会失去一种重要理解能力？

如果答案是否定的，它就不应该进入核心。

最终系统应当形成：

```text
少量精英词
    + 明确语义
    + 严格上下文约束
    + 一次性高性能索引
    + 可追溯证据
    + 中英文全本回归
    = 可维护的认知语言基础设施
```
