# 配置参考

服务器通过**环境变量**、**命令行参数**和合理的**默认值**的组合来配置。`src/config.rs` 中的 `Config` 结构体按以下优先级从所有源加载：

```
命令行参数 > 环境变量 > 默认值
```

## 快速参考

### 环境变量

| 变量 | 默认值 | 描述 |
|---|---|---|
| `MEMORY_DB_PATH` | `./memory.db` | SQLite 数据库文件路径 |
| `MEMORY_VECTOR_DIM` | `0` | 向量维度（0 = 纯关键词 FTS5） |
| `MEMORY_EMBEDDING_PROVIDER` | `none` | 嵌入后端：`none`, `openai`, `ollama` |
| `MEMORY_EMBEDDING_URL` | `http://localhost:8000` | 嵌入服务端点（用于远程提供者） |
| `MEMORY_EMBEDDING_MODEL` | `e5-large` | 嵌入模型名称 |
| `MEMORY_EMBEDDING_TIMEOUT_MS` | `30000` | 嵌入请求超时时间（毫秒） |
| `MEMORY_OPENAI_API_KEY` | — | OpenAI API Key（`openai` 提供者需要） |
| `MEMORY_RETRIEVAL_MODE` | `keyword` | 搜索模式：`keyword`, `vector`, `hybrid` |
| `MEMORY_MIN_IMPORTANCE` | `0.6` | 存储记忆的最低重要性评分 |
| `MEMORY_CONFLICT_THRESHOLD` | `0.85` | 冲突检测的余弦相似度阈值 |
| `MEMORY_MAX_SOLUTIONS` | `5000` | 每租户最大 `Knowledge` 记忆数量 |
| `MEMORY_MAX_PER_DISTILL` | `3` | 每次蒸馏调用产生的最大记忆数 |
| `MEMORY_DISABLE_CROSS_TURN` | `false` | 禁用跨轮次 4 条消息弧提取 |
| `MEMORY_SSE_ADDR` | `""` | SSE 监听地址（空 = 使用 stdio 传输） |
| `MNEMOSYNE_HOME` | — | **资源根目录**：`config/` 与 `lexicon/` 的所在位置（见下节） |
| `RUST_LOG` | — | 日志级别（例如 `info`、`debug`、`memory_distill=debug`） |

### 命令行参数

| 参数 | 环境变量 | 描述 |
|---|---|---|
| `--db-path <PATH>` | `MEMORY_DB_PATH` | 数据库文件路径 |
| `--vector-dim <N>` | `MEMORY_VECTOR_DIM` | 向量维度（0 = 关键词模式） |
| `--embedding-provider <PROVIDER>` | `MEMORY_EMBEDDING_PROVIDER` | 嵌入后端 |
| `--embedding-url <URL>` | `MEMORY_EMBEDDING_URL` | 嵌入端点 |
| `--embedding-model <MODEL>` | `MEMORY_EMBEDDING_MODEL` | 模型名称 |
| `--embedding-timeout-ms <MILLIS>` | `MEMORY_EMBEDDING_TIMEOUT_MS` | 请求超时（毫秒） |
| `--retrieval-mode <MODE>` | `MEMORY_RETRIEVAL_MODE` | 搜索模式 |
| `--min-importance <FLOAT>` | `MEMORY_MIN_IMPORTANCE` | 存储的最低重要性（默认：0.6） |
| `--conflict-threshold <FLOAT>` | `MEMORY_CONFLICT_THRESHOLD` | 冲突检测阈值（默认：0.85） |
| `--max-solutions <N>` | `MEMORY_MAX_SOLUTIONS` | 每租户最大 Knowledge 记忆数 |
| `--max-per-distill <N>` | `MEMORY_MAX_PER_DISTILL` | 每次蒸馏调用的最大记忆数 |
| `--disable-cross-turn` | `MEMORY_DISABLE_CROSS_TURN` | 禁用跨轮次提取 |
| `--sse-addr <ADDR>` | `MEMORY_SSE_ADDR` | SSE 监听地址（空 = stdio） |

## 资源文件（`config/`）

除了上面的环境变量与参数，引擎还在**资源根目录**下读取一组 JSON 词表与规则文件。
根目录按以下顺序解析，先命中者生效：

1. `MNEMOSYNE_HOME`（显式指定，最优先）；
2. 当前工作目录（若含 `config/` 或 `lexicon/`）；
3. 可执行文件所在目录（若含 `config/` 或 `lexicon/`）；
4. 兜底：当前工作目录。

| 文件 | 用途 |
|---|---|
| `config/markers_zh.json` / `markers_en.json` | 观察词表（参考样例，**升级会覆盖**） |
| `config/*.user.json` | **你自己的词表**：追加到随包表上，`_remove` 可删词 |
| `config/dictionary.json` | 核心词库：lexeme 与功能词（否定/不确定等） |
| `config/emotion_lexicon.json` | 伴随主题抽取用的情绪词 |
| `config/relation_rules.json` | 语料 ingest 的有向关系规则 |
| `config/name_validation.json` | 编译器的人名校验规则 |
| `config/faction_map.json` | 语料 ingest 的阵营映射 |
| `config/anchor_seeds.json` | 取值抽取的锚点种子 |
| `config/persona_prototypes.json` | `persona_check` 用的原型 |
| `config/persona_cards.json` | `persona_inject` 用的角色卡（可选） |
| `config/decay_config.json` | 衰减策略（可选，缺失时用内置默认） |
| `lexicon/packs/` | 词库包目录 |

词表怎么写、`_remove` 怎么用，见 README 的「自定义词表」。改完先自检：

```bash
mnemosyne config-check
```

它打印实际使用的资源根目录、加载了哪些词表文件、每个动作最终多少词、被删/被忽略/重复的条目，
以及上表每个文件是否存在；当词表不可信（非法动作名、文件无法解析）时退出码非 0。

## 配置模式

### 1. 纯关键词模式（零 API 成本）

```bash
cargo run --bin memory-mcp -- \
  --embedding-provider none \
  --retrieval-mode keyword \
  --db-path ./memory.db
```

| 设置 | 值 |
|---|---|
| 嵌入 | 禁用（无 API 调用） |
| 搜索 | FTS5 全文关键词搜索 |
| 向量维度 | 0（不使用） |
| API Key | 不需要 |

适用于：本地开发、隐私敏感环境、成本敏感部署。

### 2. 混合模式（关键词 + 向量）

```bash
MEMORY_OPENAI_API_KEY=sk-... cargo run --bin memory-mcp -- \
  --embedding-provider openai \
  --vector-dim 768 \
  --retrieval-mode hybrid
```

| 设置 | 值 |
|---|---|
| 嵌入 | OpenAI API |
| 搜索 | 混合（0.6 语义 + 0.2 关键词 + 0.2 重要性） |
| 向量维度 | 768 |
| API Key | 需要 |

适用于：检索质量重要的生产部署。

### 3. 纯向量模式

```bash
MEMORY_OPENAI_API_KEY=sk-... cargo run --bin memory-mcp -- \
  --embedding-provider openai \
  --vector-dim 768 \
  --retrieval-mode vector
```

### 4. Ollama（本地嵌入）

```bash
cargo run --bin memory-mcp -- \
  --embedding-provider ollama \
  --embedding-url http://localhost:11434 \
  --embedding-model nomic-embed-text \
  --vector-dim 768 \
  --retrieval-mode hybrid
```

## 验证规则

配置在启动时验证。常见错误：

| 条件 | 错误 |
|---|---|
| `vector_dim = 0` 且 `retrieval_mode = vector\|hybrid` | 拒绝——向量/混合搜索需要 `vector_dim > 0` |
| `provider = openai` 但未设置 `MEMORY_OPENAI_API_KEY` | 拒绝——需要 API Key |
| `min_importance` 超出 `[0, 1]` | 拒绝——必须在 0 和 1 之间 |
| `conflict_threshold` 超出 `[0, 1]` | 拒绝——必须在 0 和 1 之间 |

## 日志

服务器使用 `tracing` crate 和 `tracing-subscriber`。用 `RUST_LOG` 控制日志详细程度：

```bash
RUST_LOG=info cargo run --bin memory-mcp
RUST_LOG=debug cargo run --bin memory-mcp
RUST_LOG=memory_distill=debug cargo run --bin memory-mcp
```

## 示例：生产配置

```bash
export MEMORY_DB_PATH=/data/memory.db
export MEMORY_VECTOR_DIM=768
export MEMORY_EMBEDDING_PROVIDER=openai
export MEMORY_OPENAI_API_KEY=sk-...
export MEMORY_RETRIEVAL_MODE=hybrid
export MEMORY_MIN_IMPORTANCE=0.6
export MEMORY_MAX_SOLUTIONS=10000
export RUST_LOG=info

cargo run --bin memory-mcp
```
