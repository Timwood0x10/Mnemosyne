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
pub mod decay_tool;
pub mod external_knowledge_tools;
pub mod generalize_tool;
pub mod graph_search_tool;
pub mod http_server;
pub mod key_events_tool;
pub mod knowledge_tools;
pub mod memory_compile;
pub mod memory_transfer_tools;
pub mod persona_check_tool;
pub mod persona_inject_tool;
pub mod provenance_tool;
pub mod relationship_tool;
pub mod server;
pub mod sse;
pub mod state_timeline_tool;
pub mod story_bridge_tool;
pub mod trace_path_tool;
pub mod transport;
pub mod types;

pub use context_aware::{ContextCheckTool, context_check_definition};
pub use decay_tool::{MemoryDecayTool, memory_decay_definition};
pub use external_knowledge_tools::register_external_knowledge_tools;
pub use generalize_tool::register_generalize_tool;
pub use graph_search_tool::register_graph_search_tool;
pub use http_server::serve_http_addr;
pub use key_events_tool::{KeyEventsTool, key_events_definition};
pub use knowledge_tools::register_knowledge_tools;
pub use memory_transfer_tools::register_memory_transfer_tools;
pub use persona_check_tool::{PersonaCheckTool, persona_check_definition};
pub use persona_inject_tool::{PersonaInjectTool, persona_inject_definition};
pub use provenance_tool::{FactProvenanceTool, fact_provenance_definition};
pub use relationship_tool::{
    PersonaTimelineTool, RelationshipQueryTool, RelationshipUpdateTool,
    persona_timeline_definition, relationship_query_definition, relationship_update_definition,
};
pub use server::{MCPServer, ServerBuilder, ToolRegistry};
pub use state_timeline_tool::{StateTimelineTool, state_timeline_definition};
pub use story_bridge_tool::{StoryBridgeTool, story_bridge_definition};
pub use trace_path_tool::register_trace_path_tool;
pub use transport::{StdioTransport, Transport};
pub use types::{
    ContentBlock, Implementation, JSONRPCError, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse,
    ListToolsResult, ToolCallResult, ToolDefinition, ToolHandler,
};
