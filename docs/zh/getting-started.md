# 快速开始

## 前提条件

- **Rust 工具链**（edition 2024）：通过 [rustup](https://rustup.rs/) 安装
- **SQLite**：自动捆绑（使用 `rusqlite` 的 `bundled` 特性）
- **纯关键词模式不需要任何 API Key**

## 安装

### 从源码编译

```bash
git clone https://github.com/TimWood/memory_distill.git
cd memory_distill
cargo build --release
```

编译后的二进制文件位于 `target/release/memory-mcp`。

### 通过 Cargo 安装（发布后可用）

```bash
cargo install memory_distill
```

## 快速启动：零配置模式

无需任何外部依赖即可运行服务器——只需要 SQLite：

```bash
cargo run --bin memory-mcp -- \
  --embedding-provider none \
  --retrieval-mode keyword \
  --db-path ./my-memories.db
```

服务器以 **stdio MCP 模式**启动，在 stdin/stdout 上监听 JSON-RPC 2.0 消息，准备立即接收工具调用。

> **这里发生了什么**：SQLite FTS5 处理所有搜索。不需要 API 调用、不需要网络、不需要向量数据库。持续运行零成本。

## 验证是否正常工作

服务器运行后，发送一个 `tools/list` 请求（从另一个终端）：

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | nc -w1 localhost 8080
```

在 stdio 模式下，通常通过 MCP 客户端连接。使用 Python 辅助快速测试：

```bash
# 在后台启动服务器，捕获输出
cargo run --bin memory-mcp -- --db-path /tmp/test.db > /tmp/mcp-out &
MCP_PID=$!

# 发送 tools/list 请求
printf '{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n' > /dev/stdin
```

## 蒸馏你的第一条记忆

连接一个 MCP 客户端并调用 `memory_distill` 工具：

```json
{
  "conversation_id": "session-1",
  "messages": [
    {"role": "user", "content": "如何在 Rust 中解析 JSON？"},
    {"role": "assistant", "content": "使用 serde_json::from_str 配合类型化结构体。"}
  ]
}
```

服务器返回指标：提取了多少条经验、分类了多少、存储了多少，以及是否有任何冲突被解决。

## 搜索你的记忆

```json
{
  "query": "Rust parse JSON",
  "limit": 5
}
```

响应中包含按相关性排序的记忆列表。

## 下一步：启用嵌入（可选）

### 使用 OpenAI

```bash
MEMORY_OPENAI_API_KEY=sk-... cargo run --bin memory-mcp -- \
  --embedding-provider openai \
  --vector-dim 768 \
  --retrieval-mode hybrid
```

### 使用本地 Ollama

首先确保 Ollama 正在运行并带有嵌入模型：

```bash
ollama pull nomic-embed-text
```

然后配置服务器（具体配置参见 [configuration.md](../en/configuration.md)）。

## Claude Desktop 集成

添加到 `claude_desktop_config.json`：

```json
{
  "mcpServers": {
    "memory": {
      "command": "/path/to/memory-mcp",
      "args": [
        "--db-path", "/path/to/memory.db",
        "--retrieval-mode", "keyword"
      ]
    }
  }
}
```

项目中的示例配置：

```json
{
  "mcpServers": {
    "memory": {
      "command": "cargo",
      "args": [
        "run",
        "--bin", "memory-mcp",
        "--",
        "--db-path", "./memory.db",
        "--retrieval-mode", "keyword",
        "--embedding-provider", "none"
      ],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

## 开发命令

```bash
make check      # 运行 clippy + cargo check
make test       # 运行 142 个单元测试 + 3 个文档测试
make run        # 以 stdio MCP 模式启动服务器
make build      # 以 release 模式构建
```

## 接下来做什么？

- 了解[系统架构](architecture.md)
- 深入探索[蒸馏流水线](distillation-pipeline.md)
- 查看所有 [MCP 工具](mcp-tools.md) 及其 API
- 为你的环境配置服务器：[配置参考](configuration.md)
