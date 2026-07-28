# LoreScope — Agent 的百科辞典 + 回忆录

**两条产品线，共享 MCP 查询层：**

| | 回忆录 (Memory Distillation) | 百科辞典 (LoreScope) |
|--|----------------------------|---------------------|
| 是什么 | Agent 的自传——对话记忆 | Agent 的世界模型——知识编译 |
| 输入 | 对话消息 | 小说/历史/文档/设定 |
| 提取 | Preference, Skill, Experience, Decision | Entity, Event, Relation, Timeline |
| 存储 | `ExperienceRepository` (sqlite-vec) | `EntityStore` + `EventStore` (SQLite) |
| 查询 | `memory_search` / `memory_stats` | `inspect_entity` / `relation_graph` / `evidence` |

> **设计哲学：** Memory Distillation 回答"你记得什么？"，LoreScope 回答"世界是什么样的？"。两者合起来才是 Agent 完整的认知层——Self Memory + World Model。

---

## MCP 工具一览

### 记忆蒸馏工具（Memory Distillation — 6 个）

| 工具 | 功能 | 必填参数 |
|------|------|---------|
| `lore_scope` (原名 `memory_distill`) | 8 阶段蒸馏管线：抽取→分类→打分→过滤→压缩→向量化→冲突解决→持久化 | `conversation_id`, `messages[]` |
| `memory_compile` | 编译对话为结构化知识 + 决策 + 会话状态，可选同时蒸馏 | `messages[]` |
| `memory_search` | 关键词/向量/混合检索 | `query` |
| `memory_store` | 手动写入记忆 | `content` |
| `memory_feedback` | 记录 Agent 对记忆的反馈（用于自我进化） | `memory_id` |
| `memory_stats` | 租户维度记忆统计 | — |

### 旧版人物知识工具（V1 Legacy — 4 个）

| 工具 | 功能 | 备注 |
|------|------|------|
| `character_search` | 按姓名/属性/小说搜索人物 | 基于旧的 `character_*` 表 |
| `character_network` | 人物关系图 BFS 遍历 | V1 迁移后只读 |
| `character_ingest` | 运行四大名著语料蒸馏管线 | 触发 V1 全量提取 |
| `character_graph` | 导出 3D 人物关系图 JSON | 用于可视化 |

### 通用知识模型工具（LoreScope — 5 个）

| 工具 | 功能 | 必填参数 |
|------|------|---------|
| `inspect_entity` | 查询人物完整画像：属性 + 关系 + 事件 + 证据 | `name` |
| `timeline` | 事件时间线（按章节排序） | `entity` |
| `relation_graph` | 关系图 BFS 遍历（depth 1-5） | `entity` |
| `evidence` | 原文证据搜索（关键字匹配） | `query` |
| `correct_relation` | 校正知识图中的错误关系 | `source`, `predicate`, `old_target`, `new_target` |

**当前合计：15 个 MCP 工具**

---

## Memory Distillation：Agent 回忆录

对话记忆的完整流水线：

```
Conversation → extract problem→solution pairs → classify, score, filter
             → compress, embed, detect conflicts → persist
             → next session: search & inject into context
```

### 管线阶段

| 阶段 | 说明 |
|------|------|
| **Filter** | 去噪（短消息、闲聊）、安全检查（API key 等） |
| **Classify** | 分类：knowledge / skill / preference / experience / interaction / profile |
| **Extract** | 抽取 Problem-Solution 对，支持跨轮模式 |
| **Score** | 重要性打分 [0, 1]：基于类型偏置 + 关键词 + 长度 |
| **Compress** | "问题：方案" 格式截断（60+120 字符安全截断） |
| **Embed** | 可选向量化（OpenAI / Ollama / none） |
| **Resolve** | 余弦相似度冲突检测 → 高重要性替换低重要性 |
| **Capacity** | 租户级容量控制（默认每类 5000 条） |

### 存储

- `SQLiteVecStore`：sqlite-vec 向量索引 + FTS5 全文搜索
- 支持 `keyword` / `vector` / `hybrid` 三种检索模式
- 零 embedding 成本模式：`provider=none` → 纯 FTS5 关键词搜索

---

## LoreScope：Agent 的百科辞典

将非结构化文本编译为可验证的人物、事件、关系网络。采用 Entity-centric 的世界建模架构。

### 双 Pass 编译器

```
输入：人物简介 / 设定 / Wiki + 正文
               │
               ▼
       Document Parser
               │
       ┌───────┴───────┐
       ▼               ▼
  Pass 1:           Pass 2:
  World Builder     Story Compiler
  （建立实体节点）    （挂载事件关系）
       │               │
       └───────┬───────┘
               ▼
       Timeline Builder
               │
               ▼
       Graph Update
       │         │
       ▼         ▼
    Entity     Event
    Store      Store
```

### Pass 1：World Builder

从人物简介文本中提取 Entity Profile：

> "刘备字玄德，涿郡涿县人，中山靖王之后"  
> → Entity("刘备")
> → Profile(courtesy_name="玄德")
> → Profile(birthplace="涿郡")
> → Profile(ancestry="中山靖王")

提取的配置模式（来自 `config/entity_profiles/*.json`）：

| 模式 | Profile Key | 说明 |
|------|------------|------|
| `字XX` | `courtesy_name` | 字 |
| `XX人也` | `birthplace` | 籍贯 |
| `XX之后` | `ancestry` | 出身 |
| `身长X尺` | `appearance_height` | 身高 |
| `面如XX` | `appearance_face` | 外貌 |
| `使XX` | `weapon` | 兵器 |
| `XX为业` | `occupation` | 职业 |
| `号XX` | `title` | 称号 |

### Pass 2：Story Compiler

从正文中提取 Event 与 Relation：

> "吕布杀董卓"
> → Event(type=action, predicate=杀)
> → participants: 吕布(subject), 董卓(object)
> → Relation: 吕布 --[associated]--> 董卓

### 核心 Schema

```
entities           实体节点（人物/地点/组织）
entity_profiles    实体属性画像（字/籍贯/外貌）
events             世界状态变化
event_participants 事件参与者
relations          长期关系
evidence           原文证据
```

### 当前测试数据

> 三国演义：120 回全文处理结果
> - 人物节点: 57 （从文本自动发现）
> - 关系网络: 287 条
> - 事件: 8,233 件
> - 处理时间: ~10s

---

## Quick Start

```bash
# 零配置：仅 SQLite，无需 API key
cargo run --bin lore-scope \
  --embedding-provider none \
  --retrieval-mode keyword \
  --db-path ./knowledge.db
```

### CLI 命令

| 命令 | 功能 |
|------|------|
| `serve`（默认） | 启动 MCP stdio 服务器 |
| `ingest --corpus-dir corpus` | 运行 V1 语料蒸馏管线 |
| `migrate --corpus-dir corpus` | V1 → 通用知识模型迁移 |

### 测试

```bash
make check      # clippy + check (0 error 0 warning)
make test       # 260+ 单元测试 + 集成测试

# 全本三国演义编译测试
cargo test --test sanguo_compile e2e_sanguo -- --nocapture
```

---

## 项目结构

```
src/
├── main.rs                     # 入口 + 15 个 MCP 工具注册
├── conversation_compiler.rs    # Agent 对话编译器
│
├── compiler/                   # LoreScope 编译器
│   ├── mod.rs                  # CompileContext + IR 类型
│   ├── document.rs             # Document Parser
│   ├── sentence.rs             # Sentence Compiler
│   ├── chunk.rs                # Chunk Planner
│   ├── profile.rs              # Pass 1: World Builder
│   ├── extract.rs              # Pass 2: Story Compiler
│   ├── alias.rs                # Alias Resolver
│   ├── pronoun.rs              # Pronoun Resolver
│   ├── relation.rs             # Relation Builder
│   ├── timeline.rs             # Timeline Builder
│   ├── inference.rs            # Rule Engine
│   ├── merge.rs                # Chunk Merge
│   ├── writer.rs               # Store Writer
│   └── entity/                 # Entity Registry
│       ├── provider.rs         # EntityProvider trait
│       ├── registry.rs         # EntityRegistry
│       ├── json_provider.rs    # JSON 配置加载
│       └── ...
│
├── knowledge/                  # 存储层
│   ├── store.rs                # SQLiteKnowledgeStore
│   └── migration.rs            # V1 → 通用模型迁移
│
├── storage/
│   └── schema.rs               # 数据库 DDL
│
├── mcp/                        # MCP 框架
│   ├── server.rs               # JSON-RPC 2.0 服务器
│   ├── transport.rs            # Stdio 传输层
│   ├── types.rs                # JSON-RPC 类型
│   └── knowledge_tools.rs      # 5 个知识查询 MCP 工具
│
├── distiller.rs                # Memory Distillation 管线
├── store.rs                    # SQLiteVecStore
├── retrieval.rs                # 检索引擎
├── embed.rs                    # Embedding 服务
├── types.rs                    # Memory Distillation 类型
├── config.rs                   # 配置
├── character.rs                # V1 人物存储（Legacy）
├── ingest/                     # V1 语料处理（Legacy）
└── config/entity_profiles/     # 实体配置 JSON
```

## 配置

| 环境变量 | 默认值 | 说明 |
|---------|-------|------|
| `MEMORY_DB_PATH` | `./memory.db` | SQLite 数据库路径 |
| `MEMORY_VECTOR_DIM` | `0` | 0 = 纯关键词（FTS5），>0 = 向量搜索 |
| `MEMORY_EMBEDDING_PROVIDER` | `none` | `none` / `openai` / `ollama` |
| `MEMORY_RETRIEVAL_MODE` | `keyword` | `keyword` / `vector` / `hybrid` |
| `FACTION_MAP_PATH` | `config/faction_map.json` | 阵营映射配置 |

## 开发

```bash
make check      # cargo clippy + cargo check
make test       # 全部 260+ 测试
make fmt        # cargo fmt
```

---

## 许可

Apache-2.0
