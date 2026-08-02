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

pub mod context_aware;
pub mod external_knowledge_tools;
pub mod key_events_tool;
pub mod knowledge_tools;
pub mod memory_compile;
pub mod portrait_tool;
pub mod server;
pub mod sse;
pub mod transport;
pub mod types;

pub use context_aware::{ContextCheckTool, context_check_definition};
pub use external_knowledge_tools::register_external_knowledge_tools;
pub use key_events_tool::{KeyEventsTool, key_events_definition};
pub use knowledge_tools::register_knowledge_tools;
pub use portrait_tool::{PortraitTool, portrait_extract_definition};
pub use server::{MCPServer, ServerBuilder, ToolRegistry};
pub use transport::{StdioTransport, Transport};
pub use types::{
    ContentBlock, Implementation, JSONRPCError, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse,
    ListToolsResult, ToolCallResult, ToolDefinition, ToolHandler,
};
