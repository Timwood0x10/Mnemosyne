# 外部词典许可证审查表 (External Lexicon License Review)

> ELITE_LEXICON_PLAN §11 & P5 要求：每个外部词库包必须携带来源、版本、许可证、
> 版权声明、署名、修改记录与再分发约束。本文档是内置参考，不默认携带外部数据。

## 1. 评估标准

进入项目核心层（Core）的外部数据必须满足：

- 许可证允许再分发和修改；
- 不要求满足无法达成的前提（如禁止商用）；
- 有明确的版权声明和署名要求；
- 与项目 Apache-2.0 许可证兼容；
- 可记录来源 URL 和版本/日期。

默认禁止：牛津词典、新华字典的系统性导出、来源不明的网络词表、ShareAlike
数据未隔离前直接混入核心文件。

## 2. 审查表

| 来源 | 类型 | 许可证 | 可入 Core | 备注 |
|------|------|--------|----------|------|
| **Princeton WordNet 3.0** | 词库 | 类 BSD（WordNet License） | ✅ 经适配器 | 仅 lemma/POS/同义辅助，不决定 FactType |
| **Open English WordNet (EWN)** | 词库 | CC BY 4.0 | ✅ 经适配器 | 需署名；派生数据需同样许可 |
| **EOWL (Extended Open Word List)** | 词表 | 公共领域 (public domain) | ✅ | 仅作候选来源 |
| **SCOWL (Spell Checker Oriented Word Lists)** | 词表 | MIT + BSD 变体 | ✅ | 需保留版权声明 |
| **Moby Thesaurus** | 同义词表 | 公共领域 | ✅ | 仅候选/同义辅助 |
| **哈工大 LTP 词表** | 中文词表 | 需逐版本核实 | ⚠️ | 未核实前不入 Core |
| **现代汉语常用词表（国家语委）** | 中文词表 | 政府公开数据 | ⚠️ | 需确认再分发条款 |
| **jieba 词典** | 中文分词 | MIT | ⚠️ | 词条来源混合，需审查 |
| **牛津词典 / 新华字典** | 商业词典 | 授权制 | ❌ | 仅允许用户自备授权适配器 |
| **来源不明网络词表** | 任意 | 未知 | ❌ | 拒绝 |

## 3. 外部数据包必备字段（§11.3）

```text
source name        —— 来源名（如 "Princeton WordNet 3.0"）
source URL         —— 获取地址
source version/date—— 版本或日期
license identifier —— SPDX 标识（如 BSD-3-Clause, CC-BY-4.0）
copyright notice   —— 版权声明原文
attribution text   —— 署名文本（如包含于 NOTICE 文件）
modification record—— 修改记录（新增/删除/改动条目）
redistribution constraints —— 再分发约束（如 ShareAlike）
```

## 4. 商业词典适配器原则

- 不默认缓存或导出完整数据；
- 行为服从用户持有授权的条款；
- 仅提供接口，不内置数据（见 `src/lexicon/external.rs`）。

## 5. 当前状态

- [x] 审查表建立（本文档）
- [x] `ExternalFileProvider` 接口（不携带数据）
- [x] WordNet 数据适配样例测试（见 `src/lexicon/external.rs` 测试）
- [x] 中文开放词库评估（见 §6，2026-07-31 实测检索）

## 6. 中文开放词库实测评估（P5）

> 2026-07-31 检索 GitHub 后的结论。所有数据只经 `ExternalFileProvider` 接入，
> 仅作候选/词性/词频辅助，不决定 FactType。

### 6.1 推荐候选

| 来源 | 许可证 | 词量 | 适用性 | 结论 |
|------|--------|------|--------|------|
| **THUOCL**（清华开放中文词库） | MIT | 各领域 3k~45k | 带 DF 词频值，分 IT/财经/成语/地名等类别，明确允许商用 | ✅ **首选**：可直接经适配器接入作候选来源 |
| **qianchang/zici** | MIT | 含古汉语单字字频 | 有 `char_classical.txt` 古汉语频率表 | ✅ 对 `classical_chinese` 领域包价值高 |
| **现代汉语常用词表（国家语委）** | 公开数据 | 56,008 词 | 官方词频（2.5 亿字语料） | ⚠️ 需确认再分发条款后可用 |

### 6.2 需谨慎

| 来源 | 许可证 | 问题 |
|------|--------|------|
| **wordfreq**（rspeer） | Apache-2.0 + 数据 CC-BY-SA 4.0 | 中文词频来自 jieba 附带词表，"provenance unknown"（来源不明）；CC-BY-SA 派生数据需 ShareAlike——按 §11.2 禁止直接混入 Core |
| **lexitier-zh / HSK 列表** | MIT 等 | 面向教学（HSK/TOCFL 分级），对认知编译价值有限 |
| **Cifu** | GPL-3.0 | 粤语词库，且 GPL 与 Apache-2.0 不兼容，不适用 |

### 6.3 行动建议

1. 接入 **THUOCL_IT / THUOCL_chengyu** 作外部候选来源（MIT，DF 值可直接作频率证据）；
2. 用 **zici 古汉语字频** 验证/补充 `classical_chinese` 包（MIT）；
3. 明确**排除** wordfreq 中文部分（来源不明 + CC-BY-SA 传染）；
4. 商用词典仍只允许用户自备授权适配器。


