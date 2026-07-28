//! MCP (Model Context Protocol) server framework.
//!
//! This module ports the source project's `internal/ares_mcp/` into Rust.
//! It supports:
//!
//! - JSON-RPC 2.0 over stdio
//! - The `initialize`, `tools/list`, and `tools/call` methods
//! - A simple tool-registration API with input-schema validation
//!
//! The server is transport-agnostic at the trait level; the concrete
//! [`StdioTransport`] implementation is provided for the common case.

pub mod knowledge_tools;
pub mod server;
pub mod transport;
pub mod types;

pub use knowledge_tools::register_knowledge_tools;
pub use server::{MCPServer, ServerBuilder, ToolRegistry};
pub use transport::{StdioTransport, Transport};
pub use types::{
    ContentBlock, Implementation, JSONRPCError, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse,
    ListToolsResult, ToolCallResult, ToolDefinition, ToolHandler,
};
