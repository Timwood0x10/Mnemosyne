# Cognitive Memory MCP Server

基于 **sqlite-vec** 的对话记忆提取与检索 MCP 服务器。

## 功能

- **记忆蒸馏** — 8 阶段流水线：提取 → 分类 → 评分 → 过滤 → 嵌入 → 冲突解决 → 容量控制 → 持久化
- **混合检索** — BM25 关键词 + 向量余弦相似度 + 重要性排序
- **多租户** — 通过 `tenant_id` 隔离存储
- **可插拔嵌入** — OpenAI、Ollama 或无嵌入模式（纯关键词）
- **MCP 工具** — `memory_distill`（蒸馏）、`memory_search`（检索）、`memory_store`（存储）、`memory_feedback`（反馈）、`memory_stats`（统计）

## 架构

```
会话消息 → 蒸馏器 → 分类器 → 评分器 → 过滤器 → 嵌入器 → 冲突解决 → SQLiteVecStore
                                            ↓
                                     检索引擎（关键词 / 向量 / 混合）
```

## 快速开始

```bash
# 编译
make build

# 运行（stdio 模式，默认）
make run

# 或直接运行
cargo run --bin memory-mcp -- serve

# 运行测试
make test
```

## 配置

通过 CLI 参数或环境变量配置：

| 环境变量 | 默认值 | 说明 |
|---------|--------|------|
| `MEMORY_DB_PATH` | `./memory.db` | SQLite 数据库路径 |
| `MEMORY_VECTOR_DIM` | `1024` | 嵌入向量维度 |
| `MEMORY_EMBEDDING_PROVIDER` | `none` | `none` / `openai` / `ollama` |
| `MEMORY_EMBEDDING_URL` | `http://localhost:8000` | 嵌入服务地址 |
| `MEMORY_RETRIEVAL_MODE` | `keyword` | `keyword` / `vector` / `hybrid` |
| `MEMORY_OPENAI_API_KEY` | — | provider 为 openai 时必填 |

## 存储方案

使用 **SQLite + sqlite-vec 扩展**：
- 关系数据（记忆内容、元数据）存标准 SQLite 表
- 向量数据存 `vec0` 虚拟表，支持余弦距离近似搜索
- 无需额外服务，单文件数据库

## 蒸馏流水线

1. **提取** — 将用户/助手消息配对为 `(问题, 解答)` 元组
2. **分类** — 识别记忆类型（知识、偏好、技能等）
3. **评分** — 计算每条候选记忆的重要性分数
4. **过滤** — 丢弃低于 `min_importance` 的低价值记忆
5. **压缩** — 将 `(问题, 解答)` 压缩为一句摘要
6. **嵌入** — 生成向量用于后续冲突检测和检索
7. **冲突解决** — 余弦相似度 ≥ 阈值时，高重要性覆盖低重要性
8. **容量控制** — 超限时淘汰最低置信度的记忆

## MCP 工具

| 工具 | 说明 |
|------|------|
| `memory_distill` | 从对话中蒸馏出记忆 |
| `memory_search` | 搜索记忆（关键词/向量/混合） |
| `memory_store` | 手动存入记忆 |
| `memory_feedback` | 记录 Agent 反馈 |
| `memory_stats` | 按租户统计记忆 |

## 开发

```bash
make check    # clippy + check
make fmt      # 格式化代码
make test     # 运行测试
make clean    # 清理构建产物
```
