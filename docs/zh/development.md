# 开发指南

## 项目结构

```
memory_distill/
├── Cargo.toml              # 包清单、依赖、特性
├── Makefile                # 常用开发命令
├── src/
│   ├── main.rs             # 服务器装配、6 个工具处理器、build_server()
│   ├── lib.rs              # 包根、模块重新导出
│   ├── classifier.rs       # MemoryClassifier — 确定性关键词评分
│   ├── compiler.rs         # ConversationCompiler — 会话状态构建器
│   ├── config.rs           # Config — 命令行参数、环境变量、验证
│   ├── detector.rs         # QuestionDetector — is_problem 启发式
│   ├── distiller.rs        # PipelineDistiller — 8 阶段编排器
│   ├── embed.rs            # EmbeddingService trait + NullEmbedder + RemoteEmbedder
│   ├── error.rs            # Error — 统一错误类型
│   ├── extractor.rs        # ExperienceExtractor — 问题-解决方案对
│   ├── filter.rs           # NoiseFilter + SecurityFilter
│   ├── prompt.rs           # PromptBuilder — 结构化上下文注入
│   ├── resolver.rs         # ConflictResolver — 余弦相似度 + 重要性比较
│   ├── retrieval.rs        # RetrievalEngine — BM25、混合评分
│   ├── scorer.rs           # ImportanceScorer — [0,1] 重要性评分
│   ├── store.rs            # SQLiteVecStore — sqlite-vec + FTS5
│   ├── types.rs            # 领域类型：Memory、Experience、SessionState 等
│   └── mcp/
│       ├── mod.rs          # MCP 模块重新导出
│       ├── server.rs       # MCPServer — JSON-RPC 调度器
│       ├── transport.rs    # StdioTransport — stdin/stdout I/O
│       └── types.rs        # JSON-RPC 2.0 类型
├── docs/
│   ├── en/                 # 英文文档
│   └── zh/                 # 中文文档
└── examples/
    └── claude-desktop-config.json
```

## 开发命令

```bash
make check      # 运行：cargo clippy --all-targets --all-features + cargo check
make test       # 运行：cargo test（142 个单元测试 + 3 个文档测试）
make run        # 运行：cargo run --bin memory-mcp -- ...
make build      # 运行：cargo build --release
```

### 手动命令

```bash
# 运行所有测试并显示输出
cargo test -- --nocapture

# 运行特定测试
cargo test distill_simple_pair -- --nocapture

# 运行 clippy
cargo clippy --all-targets --all-features -- -D warnings

# 构建 release 版本
cargo build --release
```

## 测试覆盖

项目有 **142 个单元测试 + 3 个文档测试**。测试按模块组织：

| 模块 | 测试内容 |
|---|---|
| `distiller.rs` | 完整流水线、压缩、中文文本、容量控制 |
| `store.rs` | CRUD、FTS5 搜索、向量搜索、租户隔离、批量操作 |
| `retrieval.rs` | BM25 评分、分词、混合排序、租户过滤 |
| `classifier.rs` | 所有 MemoryType 分类、大小写不敏感、回退 |
| `scorer.rs` | 长度曲线、关键词上限、类型偏差排序、限幅 |
| `filter.rs` | 噪音拒绝、安全模式检测、自定义模式 |
| `resolver.rs` | 余弦相似度、冲突替换、维度不匹配 |
| `extractor.rs` | 直接/跨轮次提取、空输入、多对提取 |
| `compiler.rs` | 目标检测、文件追踪、决策、推理链 |
| `types.rs` | Memory TTL、显示文本偏好、序列化往返 |
| `config.rs` | CLI 解析、验证规则、环境变量覆盖 |
| `mcp/types.rs` | ContentBlock、ToolCallResult、JSON-RPC 消息往返 |
| `error.rs` | Display 格式化、错误类型转换 |
| `embed.rs` | RemoteEmbedder 构造 |
| `prompt.rs` | 知识/决策显示、最近消息注入 |

## 添加新功能

### 添加 MCP 工具

1. 在 `src/main.rs` 中定义工具结构体：
   ```rust
   struct MyNewTool {
       // 依赖
   }
   ```

2. 实现 `ToolHandler`：
   ```rust
   #[async_trait]
   impl ToolHandler for MyNewTool {
       async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
           // 解析参数、执行工作、返回结果
       }
   }
   ```

3. 在 `build_server()` 中注册：
   ```rust
   .register_tool("my_tool", "描述", input_schema, Arc::new(MyNewTool { ... }))
   ```

### 添加流水线阶段

1. 在 `src/distiller.rs` 中向 `PipelineDistiller` 添加阶段函数
2. 在 `distill()` 方法的正确位置插入
3. 向 `DistillationMetrics` 添加相关计数器
4. 为新阶段行为编写测试

## 代码规范

- 所有代码使用英文注释编写
- 公共函数包含带参数/返回值/错误说明的文档注释
- 测试遵循 `/// Objective: ...` + `/// Invariants: ...` 模式
- 常量使用 `SCREAMING_SNAKE_CASE`
- 在测试和 main() 之外不使用 unwrap/expect
- 错误处理：库代码用 `thiserror`，二进制代码用 `anyhow`

## 特性标志

| 特性 | 默认 | 描述 |
|---|---|---|
| `remote-embed` | 启用 | 启用基于 HTTP 的嵌入提供者（`reqwest` 依赖）。使用 `--no-default-features` 禁用，实现完全离线构建。 |

## 调试

### 日志

设置 `RUST_LOG` 获取详细输出：

```bash
RUST_LOG=debug cargo run --bin memory-mcp -- --db-path /tmp/debug.db
RUST_LOG=memory_distill=debug,rusqlite=info cargo run --bin memory-mcp
```

### 内存数据库

用于测试，避免文件 I/O：

```rust
let store = SQLiteVecStore::open_in_memory(dim).await?;
```

### 常见问题

| 问题 | 可能原因 | 解决 |
|---|---|---|
| `sqlite-vec` 初始化失败 | 缺少捆绑的扩展 | 确保 Cargo.toml 中 `sqlite-vec = "0.1"` 使用 bundled 特性 |
| 嵌入超时 | 网络或上游问题 | 增加 `--embedding-timeout` |
| FTS5 返回空结果 | 错误的 tenant_id 或空索引 | 检查 tenant_id 是否与蒸馏时使用的一致 |
| 向量维度不匹配 | 配置与存储数据不匹配 | 使用匹配的 `--vector-dim` 重新创建数据库 |
