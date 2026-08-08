# Mnemosyne

**叙事世界编译器** —— 将非结构化叙事文本编译为可查询、可验证、可回溯的结构化世界模型。

***

## 1. 项目概述

### 1.1 定位

Mnemosyne 是一个 **不依赖 LLM 的知识蒸馏引擎**。它将叙事文本（小说、历史、剧本、资料库）编译为通用知识模型，使 AI Agent、NPC、教育应用、心理陪伴系统可以查询和推理文本世界。

### 1.2 体系

| 项目            | 输入世界      | 编译产出                 |
| ------------- | --------- | -------------------- |
| **CodeScope** | 源代码       | 代码语义模型（函数/类/调用/缺陷）   |
| **ARES**      | Agent 运行时 | Agent 行为模型（工具/经验/记忆） |
| **Mnemosyne** | 叙事文本      | 叙事世界模型（实体/事件/关系/证据）  |

共同范式：**Raw World → Compiler → Semantic Model → Query / Reasoning**。

### 1.3 核心原则

| 原则                        | 说明                                             |
| ------------------------- | ---------------------------------------------- |
| **无 LLM 依赖**              | 全部由规则 + 统计 + Trie 完成；LLM 仅作可选后置增强              |
| **通用知识模型**                | 不绑定"小说"或"人物"，所有知识用 Object + Edge + Evidence 表达 |
| **证据可追溯**                 | 每条知识必须可回溯到原文位置                                 |
| **Observed / Derived 分离** | 知识分两种：原文明确的（observed）和规则推导的（derived）           |
| **SQLite 为主存**            | V1 SQLite + petgraph，暂不引入图数据库                  |

***

## 2. 系统架构（冻结版）

```
                 Text Corpus
                      |
                      v
              Lore Compiler
     ┌─────────────────────────────┐
     │  Sentence Layer             │
     │  Entity Layer               │
     │  Event Layer                │
     │  Relation Layer             │
     │  Inference Layer            │
     │  Evidence Layer             │
     └─────────────┬───────────────┘
                   |
                   v
           Knowledge Model
     ┌─────────────────────────────┐
     │  Object                     │
     │  Edge                       │
     │  Evidence                   │
     └─────────────┬───────────────┘
                   |
          ┌────────┴────────┐
          |                 |
       SQLite          petgraph
    persistence        runtime
          |                 |
          └────────┬────────┘
                   |
                   v
                MCP API
```

以后不再加层级。Compiler 的 6 层 + KM 的 3 元素 + 2 种存储 + 1 个 API 入口，冻结。

***

## 3. 数据模型（冻结版）

### 3.1 最终表清单

| 表                    | 作用                          | 必需   |
| -------------------- | --------------------------- | ---- |
| `documents`          | 文档元数据                       | ✅    |
| `chapters`           | 章节内容                        | ✅    |
| `knowledge_objects`  | 实体（人物/事件/地点/概念...）          | ✅    |
| `knowledge_edges`    | 关系边（带时空 + observed/derived） | ✅    |
| `knowledge_evidence` | 多对多：知识 ↔ 证据                 | ✅    |
| `evidence`           | 原文证据记录                      | ✅    |
| `mentions`           | 实体出现位置索引                    | ✅    |
| `compiler_runs`      | 编译构建记录（版本追踪）                | ✅    |
| `sentences`          | 句子（可选优化）                    | ❌ 可选 |

### 3.2 documents

```sql
CREATE TABLE documents (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    author TEXT,
    doc_type TEXT,
    created_at INTEGER DEFAULT (strftime('%s','localtime'))
);
```

### 3.3 chapters

```sql
CREATE TABLE chapters (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id INTEGER NOT NULL,
    chapter_no INTEGER NOT NULL,
    title TEXT,
    content TEXT,
    start_offset INTEGER,
    end_offset INTEGER,
    FOREIGN KEY(doc_id) REFERENCES documents(id)
);
```

### 3.4 knowledge\_objects

**不再加字段**。超出 type/name/properties 的一切属性放入 properties JSON。

```sql
CREATE TABLE knowledge_objects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id INTEGER NOT NULL,
    object_type TEXT NOT NULL,     -- person / event / place / organization / concept / artifact / role
    name TEXT NOT NULL,
    properties JSON DEFAULT '{}',  -- ALL extra fields here: aliases, faction, appearance, personality...
    confidence REAL DEFAULT 1.0,
    created_at INTEGER DEFAULT (strftime('%s','localtime')),
    FOREIGN KEY(doc_id) REFERENCES documents(id)
);
```

不创建 `CharacterObject` / `EventObject` 等子表。

### 3.5 knowledge\_edges

```sql
CREATE TABLE knowledge_edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id INTEGER NOT NULL,
    target_id INTEGER NOT NULL,
    predicate TEXT NOT NULL,          -- serves / kills / trusts / participated_in / located_in
    properties JSON DEFAULT '{}',     -- weight, confidence, etc.
    origin TEXT CHECK(origin IN ('observed','derived')) DEFAULT 'observed',
    confidence REAL DEFAULT 1.0,
    valid_from INTEGER,               -- 关系起始时间（chapter_no）
    valid_to INTEGER,                 -- 关系结束时间（NULL = 持续至今）
    created_at INTEGER DEFAULT (strftime('%s','localtime')),
    FOREIGN KEY(source_id) REFERENCES knowledge_objects(id),
    FOREIGN KEY(target_id) REFERENCES knowledge_objects(id)
);
```

**Temporal 是关键设计决策**：叙事世界中关系会变化（吕布→丁原：serves ch1 → kills ch3）。不用 temporal 会导致查询时间污染。

### 3.6 evidence

```sql
CREATE TABLE evidence (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id INTEGER NOT NULL,
    chapter_id INTEGER NOT NULL,
    start_offset INTEGER,
    end_offset INTEGER,
    content TEXT,                    -- 原文片段
    created_at INTEGER DEFAULT (strftime('%s','localtime')),
    FOREIGN KEY(doc_id) REFERENCES documents(id),
    FOREIGN KEY(chapter_id) REFERENCES chapters(id)
);
```

**Evidence 不知道它服务谁** —— 通过中间表关联。

### 3.7 knowledge\_evidence（多对多中间表）

```sql
CREATE TABLE knowledge_evidence (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_type TEXT NOT NULL,        -- 'object' / 'edge'
    source_id INTEGER NOT NULL,      -- knowledge_objects.id / knowledge_edges.id
    evidence_id INTEGER NOT NULL,
    FOREIGN KEY(evidence_id) REFERENCES evidence(id),
    UNIQUE(source_type, source_id, evidence_id)
);
```

一句话支持多个事实：

```
文本："赵云单骑救阿斗，刘备大喜，称其忠勇。"

证据 id=1 → 支持：
  - object(赵云)       ← 人物出现
  - edge(赵云→阿斗, rescued)  ← 救援事件
  - edge(刘备→赵云, trusts)   ← 信任关系
```

### 3.8 mentions

```sql
CREATE TABLE mentions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    object_id INTEGER NOT NULL,
    chapter_id INTEGER NOT NULL,
    start_offset INTEGER,
    end_offset INTEGER,
    alias_used TEXT,
    confidence REAL DEFAULT 1.0,
    FOREIGN KEY(object_id) REFERENCES knowledge_objects(id),
    FOREIGN KEY(chapter_id) REFERENCES chapters(id)
);
```

Entity Resolution 的索引。赵云出现位置：ch3 offset 100, ch41 offset 900, ch71 offset 300。

### 3.9 compiler\_runs

```sql
CREATE TABLE compiler_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id INTEGER NOT NULL,
    version TEXT NOT NULL,            -- lore-compiler v0.1.0
    started_at INTEGER,
    finished_at INTEGER,
    status TEXT,                      -- running / completed / failed
    statistics JSON,                  -- objects_count, edges_count, evidence_count
    FOREIGN KEY(doc_id) REFERENCES documents(id)
);
```

类似编译器 build 记录。版本可比较：v0.1 vs v0.2 覆盖率和准确率变化。

***

## 4. 核心技术路线

### 4.1 Entity Resolution —— Trie + Alias Graph

不靠 NLP NER。用 **Trie** 做 O(n) 别名匹配：

```
Trie:
  关 → 关羽 (关公/关某/关将军)
  云 → 赵云
  云长 → 关羽 (最长优先)
```

单字符别名（"飞曰"→张飞）加边界检查：前非汉字 + 后跟动词。

### 4.2 Event Frame —— 谓词中心

不抽"事件名称"，抽 `predicate + arguments`：

```
"赵云单骑救出阿斗"
→ predicate: "救"
→ arguments: [{actor: 赵云}, {target: 阿斗}, {manner: 单骑}]
```

### 4.3 Relation Inference —— Rule Engine

类比 CodeScope。observed 靠 dialog chain + proximity，derived 靠规则推导：

```rust
Rule {
    name: "君臣信任推导",
    pattern: [
        Edge(predicate="rescued", actor=X, target=Y),
        Edge(source=Z, target=Y, predicate="parent_of"),
    ],
    derive: Edge(source=Z, target=X, predicate="trusts", confidence=0.7),
}
```

***

## 5. MCP 工具（冻结版）

### inspect\_entity

```json
{"name": "赵云", "doc": "三国演义"}

→ {type, properties, events, relations, evidences}
```

### timeline

```json
{"entity": "赵云", "doc": "三国演义"}

→ [{chapter, event, predicate, target}, ...]
```

### relation\_graph

```json
{"entity": "赵云", "depth": 2}

→ {nodes: [...], edges: [...], temporal: {valid_from, valid_to}}
```

### evidence

```json
{"query": "赵云 忠诚"}

→ [{text, chapter, doc, confidence}, ...]
```

***

## 6. 冻结决议

### ✅ 冻结内容

| 项                 | 决策                                                              |
| ----------------- | --------------------------------------------------------------- |
| 项目定位              | Mnemosyne: Narrative World Compiler                             |
| 知识模型              | Object + Edge + Evidence，不加领域子类                                 |
| Edge 时间           | valid\_from / valid\_to 必加                                      |
| Evidence 关联       | 中间表 knowledge\_evidence，多对多                                     |
| 存储                | SQLite + petgraph，不上图数据库                                        |
| Compiler pipeline | 6 层：Sentence → Entity → Event → Relation → Inference → Evidence |
| MCP 接口            | inspect\_entity / timeline / relation\_graph / evidence         |
| 护城河               | Evidence-backed Knowledge Compilation，非 LLM extraction          |

### ❌ 暂缓内容

| 项              | 原因                  |
| -------------- | ------------------- |
| 图数据库           | 问题不在存储，在 extraction |
| LLM extraction | 会变成 prompt 工程，无壁垒   |
| 自动人格分析         | V2，等 compiler 稳定    |
| 大规模推理系统        | V3                  |
| UI / 可视化       | 等 API 稳定            |

### 迁移策略

**不双写**。V1 domain 表（characters/relations/events）作为 legacy view 保留读取能力，但新数据只写入通用表。

路线：

```
V1 existing code (domain model)
    ↓ 一次性 migration
通用表 (knowledge_objects / knowledge_edges / evidence)
    ↓
MCP 查询
```

***

## 7. Phase 0 目标

下一步不是设计，是工程：

1. `src/knowledge/mod.rs` — Rust struct 定义（Object / Edge / Evidence / Mention）
2. `storage/schema.rs` — 建表 SQL migration
3. 一次性 migration：将三国 V1 数据从 domain 表转入通用表
4. `inspect_entity("赵云")` 跑通，返回 Object + Edges + Evidence

如果这个跑通，Mnemosyne 就已经是一个真正的产品雏形。

***

> **文档版本**：V2.0（冻结版）—— 停止设计，进入实现。

