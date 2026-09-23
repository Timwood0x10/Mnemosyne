# Mnemosyne — 记忆蒸馏引擎

> **Mnemosyne**（希腊记忆女神摩涅莫绪涅）—— 记忆不死、可重构、跨会话延续。

把任意长期交互编译为**可演化的人类认知模型**（身份、偏好、目标、情绪、关系）。
专为陪伴型 AI 设计，跨会话保持人设一致。

> **事实来自编译，而非猜测。**
> **状态来自事件，而非提示词。**
> **长期认知来自模型，而非上下文窗口长度。**

---

## 关于命名

**Mnemosyne**（摩涅莫绪涅）是希腊记忆女神——九位缪斯之母，掌管一切不可遗忘之物。
这个名字是本引擎的承诺：

- **记忆不死** — 事实持久化于 SQLite，跨会话存活，与上下文窗口无关；
  `memory_export` / `memory_import` 可完整备份、跨机器迁移。
- **可重构** — 每条事实携带证据链（EvidenceRef）与评分规则，人设演化时间线
  可随时从原始历史重建——无猜测，皆可追溯。
- **跨会话延续** — 认知"在模型里，不在提示词里"：存储而非上下文窗口才是事实来源。

正如 Mnemosyne 让诗人与英雄*铭记*，本引擎让陪伴型 AI **确定性、无 LLM** 地记住它的用户。

---

## 核心架构

```
                语言前端 (Language Frontend)
                     │
                     ▼
             观察编译器
                     │
                     ▼
             知识编译器
                     │
                     ▼
              快照构建器
                     │
                     ▼
              认知上下文
```

### 编译管线

| 阶段 | 说明 |
|-------|------|
| **语言前端** | 中英文自然语言解析；提取 Mentions / Actions / Evidence |
| **观察编译器** | 统一 IR（主语 + 动作 + 宾语 + 证据），Aho-Corasick 动词匹配 |
| **知识编译器** | 观察 → 事实（不可变、持久化）。FactType：Identity / Preference / Goal / Event / Relationship / Emotion / Location / Occupation / Interest / Habit |
| **快照构建器** | 事实 → StateEngine → EntitySnapshot（Markdown / JSON） |
| **认知上下文** | 向 AI Agent 提供结构化认知快照上下文 |

### 核心理念

- **事实来自编译**：所有事实由编译器从原文提取并带证据链（EvidenceRef）追踪——无 LLM 猜测。
- **状态来自事件**：实体当前状态由其事实时间线聚合而来，而非临时拼 prompt。
- **长期认知来自模型**：认知状态持久化于 SQLite，跨会话演化，与上下文窗口大小无关。

> 模块级详细文档：参见 [模块文档](#模块文档)。

---

## MCP 工具一览

### 认知状态工具

| 工具 | 功能 | 必填参数 |
|------|----------|-----------------|
| `memory_compile` | 把对话编译为结构化事实 + 认知状态，可选蒸馏 | `messages[]` |
| `cognitive_context` | 查询实体认知快照：身份 / 偏好 / 目标 / 事件 / 关系 | `name` |
| `memory_context_check` | 主动上下文感知蒸馏：超过阈值自动编译对话为事实 + 长期记忆 + 用户画像；低于阈值则为只读诊断 | `messages[]` |
| `state_timeline` | 还原实体状态如何演化：分维度状态区间（生效窗口 + 证据）与确定性变迁（`gradual_change`/`stance_flip`/`behavioral_confirmation`） | `entity_id`, `dimension?`, `tenant_id?` |
| `fact_provenance` | 审计一条事实为何成立：置信度、认知状态（active/superseded/contradicted）、原文证据、`derived_from` 推导链 | `fact_id`, `tenant_id?` |

### 决策工具

一等公民的决策：谁承诺了什么、依据是什么、后续结果如何。决策由**编译**产生——
`memory_compile` 把明确的承诺（"我答应…"）编译为 `Decision` 记录，并先落一条承载该话语的
Event 事实作为锚点——再由以下两个工具读回。

| 工具 | 功能 | 必填参数 |
|------|----------|-----------------|
| `decision_trace` | 回溯决策依据的支持事实（supporting evidence，非因果） | `decision_id`, `tenant_id?` |
| `decision_search` | 按关键词检索某主体的决策（verb/object），最新优先 | `subject`, `keyword?`, `limit?`, `tenant_id?` |

### 记忆蒸馏工具

| 工具 | 功能 | 必填参数 |
|------|----------|-----------------|
| `lore_scope` | 8 阶段流水线：提取 → 分类 → 评分 → 过滤 → 压缩 → 嵌入 → 冲突解决 → 持久化 | `conversation_id`, `messages[]` |
| `memory_search` | 关键词 / 向量 / 混合检索 | `query` |
| `memory_store` | 手动写入记忆 | `content` |
| `memory_feedback` | 记录 Agent 对记忆的反馈（用于自演化） | `memory_id` |
| `memory_stats` | 租户级记忆统计 | — |
| `memory_decay` | 确定性记忆衰减/遗忘（降权 + 归档过时事实，永不删除） | — |

### 知识摄入 / 外部源工具

| 工具 | 功能 | 必填参数 |
|------|----------|-----------------|
| `generalize_compile` | 编译任意外部数据（对话 `doc_type=dialog` 或散文 `doc_type=text`）为统一知识图谱（文档/实体/边/证据） | `messages[]` 或 `text` |
| `agent_fact_compile` | 把 AI 对话编译为三态事实（支持/反驳/无关） | `messages[]` |
| `knowledge_attach` | 挂载外部知识源（PDF/JSON/TXT/MD 文档或 JSON 数据库）为可检索适配器 | `source_name`, `path` |
| `knowledge_ingest` | 物化已挂载的外部源为文档 + 章节 + 证据 | `source_name` |
| `memory_export` | 把整个知识图谱序列化为可移植 JSON 快照（内联或写入 `path`） | — |
| `memory_import` | 回放 `memory_export` 快照，按身份去重 | `content` 或 `path` |

### Mnemosyne 知识查询工具

| 工具 | 功能 | 必填参数 |
|------|----------|-----------------|
| `inspect_entity` | 查询完整实体画像：属性 + 关系 + 事件 + 证据 | `name` |
| `timeline` | 事件时间线（按章节排序） | `entity` |
| `relation_graph` | 关系图 BFS 遍历（深度 1-5） | `entity` |
| `evidence` | 原文证据搜索（关键词匹配） | `query` |
| `correct_relation` | 修正知识图谱中的错误关系 | `source`, `predicate`, `old_target`, `new_target` |
| `person_key_events` | 提炼人物轨迹的关键事件（评分 + 证据） | `name` |
| `search_graph` | 结构化图搜索：名称子串 / 对象类型 / 属性值，可按文档限定 | `query` |
| `trace_path` | 两个命名实体间的最短关系路径（图边 BFS） | `source`, `target`, `max_depth` |

### 陪伴人设工具

陪伴型 AI 的"人设不崩"工具：注入结构化人设卡、守护草稿回复与已积累人设的一致性、
追踪 Agent↔用户关系、重建人设演化时间线。

| 工具 | 功能 | 必填参数 |
|------|----------|-----------------|
| `persona_inject` | 向系统提示注入结构化、确定性人设卡（身份/人设/风格/禁忌/关系）；按 `tenant_id` 多租户 | `agent_id` |
| `persona_check` | 守护草稿回复与已积累人设事实的一致性，报告 `conflicts` + `drift`（无 LLM，关键词回退） | `agent_id`, `draft` |
| `relationship_update` | 从消息情绪信号增量更新 Agent↔用户关系状态（亲密度/阶段/趋势/近期话题） | `agent_id`, `user_id`, `messages[]` |
| `relationship_query` | 读取租户/Agent/用户三元组的当前关系快照 | `agent_id`, `user_id` |
| `persona_timeline` | 从积累的事实重建实体人设演化时间线（`起点 → 关键转变点 → 现状`，mem0 v3 ADD-only） | `entity_id` |
| `story_bridge` | 小说角色桥接：把主角的知识图谱故事事件编译为人设事实，为 `persona_timeline`/`persona_check` 提供冷启动基线 | `name` |
| `memory_decay` | 确定性记忆衰减/遗忘（降权 + 归档过时事实，永不删除，保护高价值人设事实） | — |

### V1 遗留角色工具

| 工具 | 功能 | 说明 |
|------|----------|------|
| `character_search` | 按名称/属性/小说搜索角色 | 迁移后只读；基于旧 `character_*` 表 |
| `character_network` | 角色关系图 BFS | 迁移后只读 |
| `character_ingest` | 运行四大名著语料蒸馏流水线 | 触发完整 V1 提取 |
| `character_graph` | 导出 3D 角色关系图 JSON | 用于可视化 |

> `portrait_extract`（简历 → 人物画像）已移除，改用认知-事实对话流水线，
> 这是陪伴型 AI 人设建模的受支持路径。

---

## 无 LLM 的语义近似

系统在检索与人设推理上刻意不依赖 LLM：所有语义近似都用确定性、离线算法完成，
零 API key 依赖、行为可复现。按语义覆盖能力递增：

### 1. 关键词匹配 — FTS5

`MEMORY_VECTOR_DIM=0`（默认）启用 SQLite FTS5 关键词搜索，Unicode 安全、
LIKE 通配符已转义。精确与子串匹配直接走索引——O(log n)，无需嵌入。

### 2. 向量检索 — 余弦相似度

当 `MEMORY_VECTOR_DIM>0` 且配置了嵌入 provider（`MEMORY_EMBEDDING_PROVIDER=openai|ollama`，远程 API）时，文本嵌入后按余弦相似度查询：

- `brute_force.rs` — 精确 O(N) 全扫（ground truth）；
- `hnsw.rs` — 大图近似最近邻；对退化输入有约定：零向量返回距离 `sqrt(2)`
  （cosine = 0.0），与 brute_force 一致，两个索引结论相同。

### 3. 混合检索

`MEMORY_RETRIEVAL_MODE=hybrid` 合并关键词命中与向量命中；无向量条目
（如旧数据）回退关键词评分，检索路径绝不静默丢弃缺嵌入的记录。

### 4. 名称与别名的字符 bigram Jaccard

实体名称匹配（`compiler/resolver.rs`）把每个名字嵌入为**字符 bigram 集合**
（如 `张三` → `{张三}`），相似度 = 两集合的 **Jaccard 重叠**——无需向量模型。
可确定性地捕获转写/抄写错误与部分名称别名。

### 5. 人设一致性的共享 bigram 启发式

`persona_check`（无 LLM 路径）用**否定检测 + 共享 bigram 重叠**比较草稿回复与
已存人设事实：同一 bigram 主题的两条陈述被判为冲突/漂移——无需任何模型。
`我讨厌应酬` 与 `我喜欢安稳` 因共享 bigram 少于 2 个，正确**不**被判为立场翻转。

### 6. 规则驱动编译

整个提取流水线是规则而非推理：Aho-Corasick 动词匹配（`observation_compiler`）、
确定性事件/关系构建（`compiler/extract.rs`、`timeline.rs`）、关键事件重要性评分
（证据丰富度 + 参与者中心度 + 转折点标记）、基于规则的关系状态更新
（`relationship_update`）全部离线运行、完全可复现。

> 净效果：每条事实、每条边、每个检索结果都携带可追溯的证据链或评分规则——
> "无 LLM 猜测"的保证自始至终成立。

---

## IDE 接入（MCP）

服务器讲标准 MCP（stdio 或 HTTP+SSE），可接入任何支持 MCP 客户端的 IDE/Agent——
Claude Desktop、Cursor、VS Code、JetBrains 或自定义 Agent 框架。

### stdio（本地，IDE 推荐）

```jsonc
// .mcp.json（VS Code / Cursor / Claude Desktop 风格）
{
  "mcpServers": {
    "mnemosyne": {
      "command": "cargo",
      "args": ["run", "--bin", "mnemosyne", "serve"],
      "cwd": "/abs/path/to/memory_distill"
    }
  }
}
```

> 提示：`cargo run` 首次连接会编译。要获得流畅体验，先
> `cargo build --release`，再把 `command` 指向 `target/release/mnemosyne`，
> `args: ["serve"]`。

### HTTP+SSE（远程，需 token）

HTTP 传输暴露两个端点：`GET /sse`（Server-Sent Events 流）与 `POST /message`
（JSON-RPC）。MCP 客户端请配置 SSE 端点 URL：

```jsonc
{
  "mcpServers": {
    "mnemosyne": {
      "url": "http://host:5609/sse",
      "headers": {
        "Authorization": "Bearer <your-token>",
        "x-mcp-session-id": "<stable-id-per-client>"
      }
    }
  }
}
```

HTTP 服务未提供 `--http-token` 时拒绝启动（见 [配置](#配置)）；每个并发客户端
应发送稳定的 `x-mcp-session-id`，使其 SSE 流只收到自己的响应。远程部署请使用
`https://`——否则 bearer token 将以明文在网络上传送。

### 接入后 AI 能做什么

- **蒸馏当前对话**：宿主把进行中的 `messages[]` 传给 `memory_compile` /
  `memory_context_check`，得到结构化事实、决策、会话状态与（可选）长期蒸馏记忆——
  持久化到 SQLite 实现跨会话延续。
- **查询知识图谱**：`inspect_entity`、`timeline`、`relation_graph`、
  `search_graph`、`trace_path`、`evidence`。
- **守护人设一致性**：`persona_check` / `persona_inject` /
  `relationship_update` 让陪伴型 AI 的人设不崩。

> 注意：MCP 工具由宿主调用——服务器从不自动读取对话。要实现每轮自动蒸馏，
> 请在宿主（system prompt 或 post-turn hook）中指示它用当前 messages 调用
> `memory_compile`。

---

## 安装

### 方式一：安装脚本（推荐）

`scripts/install.sh` 自动检测你的平台/架构，下载对应的 Release 压缩包，并解压到 `.mnemosyne/` 目录：

```bash
# macOS / Linux（bash）
curl -fsSL https://raw.githubusercontent.com/Timwood0x10/Mnemosyne/main/scripts/install.sh | bash

# 或 clone 后在本地运行
git clone https://github.com/Timwood0x10/Mnemosyne.git
cd Mnemosyne
./scripts/install.sh          # 安装最新版本
./scripts/install.sh v0.1.2   # 安装指定版本
```

安装后的目录结构——所有文件放在同一目录，二进制启动时自动在自身旁边找到资源：

```
~/.mnemosyne/
├── mnemosyne            # 二进制（Windows 为 mnemosyne.exe）
├── markers_zh.json      # 中文观察词表（可编辑）
└── markers_en.json      # 英文观察词表（可编辑）
```

运行：

```bash
~/.mnemosyne/mnemosyne serve
```

### 方式二：手动下载

从 [Releases](https://github.com/Timwood0x10/Mnemosyne/releases) 页面下载对应平台的压缩包：

| 平台 | 压缩包 |
|---|---|
| macOS（Apple Silicon） | `mnemosyne-aarch64-apple-darwin.tar.gz` |
| macOS（Intel） | `mnemosyne-x86_64-apple-darwin.tar.gz` |
| Linux（arm64） | `mnemosyne-aarch64-unknown-linux-gnu.tar.gz` |
| Linux（x86_64） | `mnemosyne-x86_64-unknown-linux-gnu.tar.gz` |
| Windows（x86_64） | `mnemosyne-x86_64-pc-windows-msvc.tar.gz` |

```bash
# macOS / Linux
mkdir -p ~/.mnemosyne && tar -xzf mnemosyne-<平台>.tar.gz -C ~/.mnemosyne
chmod +x ~/.mnemosyne/mnemosyne
~/.mnemosyne/mnemosyne --version

# Windows：用解压工具解压，然后运行 mnemosyne.exe
```

### 自定义词表

`markers_zh.json` 和 `markers_en.json` 决定对话中哪些词能产出事实。每个文件把
动作（`feel`、`plan`、`want`、`dislike`、`belief`、`stuck`、`life_event` 等）
映射到一组触发词。你可以：

- 添加自己的词汇（网络流行语、领域术语、个人习惯表达）；
- 删除产生误判的词；
- 调整引擎从对话中提取用户信息的重点。

无需重新编译——直接编辑 JSON 后重启即可。文件缺失时二进制自动使用内置默认词表。

### 方式三：源码构建

需要 Rust 工具链（MSRV 见 `rust-toolchain` / Cargo.toml）。

```bash
git clone https://github.com/Timwood0x10/Mnemosyne.git
cd Mnemosyne
cargo build --release
./target/release/mnemosyne --version
```

然后继续下面的[快速启动](#快速启动)。

---

## 快速启动

```bash
# 零配置：只要 SQLite，不需要 API key
cargo run --bin mnemosyne \
  --embedding-provider none \
  --retrieval-mode keyword \
  --db-path ./knowledge.db
```

### CLI 命令

| 命令 | 说明 |
|---------|-------------|
| `serve`（默认） | 启动 MCP stdio 服务器 |
| `ingest --corpus-dir corpus` | 运行 V1 语料蒸馏流水线 |
| `migrate --corpus-dir corpus` | 迁移 V1 → 通用知识模型 |

### 测试

```bash
make check      # cargo clippy + cargo check（0 error）
make test       # 780+ 单元 + 集成测试（nextest，热缓存约 1s）
```

测试套件**自包含**：每个用例自建合成语料与内存 SQLite，全新 checkout 即可通过，
不需要任何 fixture，也不依赖网络。认知层的端到端测试走**真实 MCP JSON-RPC 路径**
（`tools/call` over 内存传输），而不是直接调用 handler：

```bash
cargo test --test cognitive_state_e2e -- --nocapture
```

### 本地语料（可选）

`corpus/` 存放体积较大的第三方文本，用于**手工 / 临时**验证
（`ingest --corpus-dir corpus`、`migrate --corpus-dir corpus`）。该目录已被 gitignore，
且**默认测试套件不依赖它**——依赖语料的回归测试已移除，CI 不会因缺 fixture 失败。

| 语料 | 语言 | 类型 |
|--------|----------|------|
| `三国演义.txt` | 中 | 小说文本 |
| `水浒传.txt` | 中 | 小说文本 |
| `红楼梦.txt` | 中 | 小说文本 |
| `西游记.txt` | 中 | 小说文本 |
| `封神演义.txt` | 中 | 小说文本 |
| `大秦帝国.txt` | 中 | 小说文本 |
| `倾城之恋.txt` | 中 | 中篇文本 |
| `WarandPeace.txt`（战争与和平） | 英 | 小说文本 |
| `PrideAndPrejudice.txt`（傲慢与偏见） | 英 | 小说文本 |
| `巴黎圣母院.pdf` / `1.pdf` / `2.pdf` | 中 / — | PDF |
| `bailiusu_escape.json`（白流苏逃出白家） | 中 | 对话 |
| `warpeace_pierre.json`（战争与和平 · 皮埃尔） | 英 | 对话 |
| `raskolnikov_porfiry.json`、`sonia_raskolnikov.json` | 中 | 对话 |
| `conversation_export_2026-08-02.json` | 中 | 对话导出 |
| `ques.json` | 中 | 辅助 |

实体画像包（`config/entity_profiles/`）：`sanguo.json`、`shuihu.json`、
`honglou.json`、`xiyou.json`、`fengshen.json`、`warandpeace.json`——每部小说
为编译器词典（`JsonEntityProvider`）提供规范实体名/别名。

---


## 模块文档

| 模块 | 中文 | English |
|------|------|---------|
| 系统架构 | [docs/zh/architecture.md](docs/zh/architecture.md) | [docs/en/architecture.md](docs/en/architecture.md) |
| 叙事编译流水线 | [docs/zh/compiler.md](docs/zh/compiler.md) | [docs/en/compiler.md](docs/en/compiler.md) |
| MCP 框架与工具 | [docs/zh/mcp.md](docs/zh/mcp.md) | [docs/en/mcp.md](docs/en/mcp.md) |
| 知识存储层 | [docs/zh/knowledge.md](docs/zh/knowledge.md) | [docs/en/knowledge.md](docs/en/knowledge.md) |
| 认知层 | [docs/zh/cognition.md](docs/zh/cognition.md) | [docs/en/cognition.md](docs/en/cognition.md) |
| 检索层 | [docs/zh/retrieval.md](docs/zh/retrieval.md) | [docs/en/retrieval.md](docs/en/retrieval.md) |

---

## 配置

| 环境变量 | 默认值 | 说明 |
|---------|---------------|-------------|
| `MEMORY_DB_PATH` | `./memory.db` | SQLite 数据库路径 |
| `MEMORY_VECTOR_DIM` | `0` | 0 = 纯关键词（FTS5），>0 = 向量检索 |
| `MEMORY_EMBEDDING_PROVIDER` | `none` | `none` / `openai` / `ollama` |
| `MEMORY_RETRIEVAL_MODE` | `keyword` | `keyword` / `vector` / `hybrid` |
| `FACTION_MAP_PATH` | `config/faction_map.json` | 阵营映射配置 |

---

## 开发

```bash
make check      # cargo clippy + cargo check
make test       # 全部 780+ 测试
make fmt        # 格式化代码
```

---

## 许可

Apache-2.0
