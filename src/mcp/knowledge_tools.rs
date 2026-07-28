//! MCP tool handlers for the general knowledge model (dev_guide §5).
//!
//! Registers four read-only query tools — `inspect_entity`, `timeline`,
//! `relation_graph`, and `evidence` — that delegate to [`KnowledgeStore`]
//! methods. All handlers serialize results as JSON text blocks.
//!
//! # Tool inventory
//!
//! | Tool              | Required     | Optional  | Delegate                      |
//! |-------------------|--------------|-----------|-------------------------------|
//! | `inspect_entity`  | `name`       | `doc`     | `store.inspect_entity()`      |
//! | `timeline`        | `entity`     | `doc`     | `store.entity_timeline()`     |
//! | `relation_graph`  | `entity`     | `doc`, `depth` | `store.relation_graph()` |
//! | `evidence`        | `query`      | `doc`, `limit` | `store.search_evidence()` |

use std::sync::Arc;

use serde_json::Value;

use crate::error::Error;
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::SQLiteKnowledgeStore;
use crate::mcp::server::ServerBuilder;
use crate::mcp::types::{ContentBlock, ToolCallResult, ToolDefinition, ToolHandler};

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Extract a required `&str` argument from the JSON args object.
fn req_str(args: &Value, key: &str) -> Result<String, Error> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| Error::InvalidInput(format!("missing required argument `{key}`")))
}

/// Extract an optional `&str` argument from the JSON args object.
fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

/// Extract an optional `usize` argument with a default fallback.
fn opt_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64().map(|u| u as usize))
        .unwrap_or(default)
}

/// Build a success [`ToolCallResult`] containing a single JSON text block.
fn json_ok(value: &impl serde::Serialize) -> Result<ToolCallResult, Error> {
    let text = serde_json::to_string_pretty(value).map_err(|e| {
        Error::Internal(format!("serialize result: {e}"))
    })?;
    Ok(ToolCallResult {
        content: vec![ContentBlock {
            block_type: "text".into(),
            text: Some(text),
            mime_type: Some("application/json".into()),
        }],
        is_error: false,
    })
}

/// Build an error [`ToolCallResult`] whose content is a human-readable message.
fn err_result(msg: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock {
            block_type: "text".into(),
            text: Some(msg.into()),
            mime_type: None,
        }],
        is_error: true,
    }
}

// ── inspect_entity ──────────────────────────────────────────────────────────

struct InspectEntityHandler {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait::async_trait]
impl ToolHandler for InspectEntityHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let name = req_str(args, "name")?;
        let doc = opt_str(args, "doc");

        match self.store.inspect_entity(&name, doc).await? {
            Some(result) => json_ok(&result),
            None => Ok(err_result(format!("entity `{name}` not found"))),
        }
    }
}

// ── timeline ────────────────────────────────────────────────────────────────

struct TimelineHandler {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait::async_trait]
impl ToolHandler for TimelineHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let entity = req_str(args, "entity")?;
        let doc = opt_str(args, "doc");
        let entries = self.store.entity_timeline(&entity, doc).await?;
        json_ok(&entries)
    }
}

// ── relation_graph ──────────────────────────────────────────────────────────

struct RelationGraphHandler {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait::async_trait]
impl ToolHandler for RelationGraphHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let entity = req_str(args, "entity")?;
        let doc = opt_str(args, "doc");
        let depth = opt_usize(args, "depth", 2).clamp(1, 5);

        match self.store.relation_graph(&entity, depth, doc).await? {
            Some(result) => json_ok(&result),
            None => Ok(err_result(format!("entity `{entity}` not found"))),
        }
    }
}

// ── evidence ────────────────────────────────────────────────────────────────

struct EvidenceHandler {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait::async_trait]
impl ToolHandler for EvidenceHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let query = req_str(args, "query")?;
        let doc = opt_str(args, "doc");
        let limit = opt_usize(args, "limit", 20);
        let hits = self.store.search_evidence(&query, doc, limit).await?;
        json_ok(&hits)
    }
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Register the four general-knowledge MCP tools on `builder`.
///
/// Each tool wraps a [`KnowledgeStore`] method and speaks JSON-RPC 2.0 over
/// the existing [`ServerBuilder`] infrastructure.
pub async fn register_knowledge_tools(
    builder: ServerBuilder,
    store: Arc<SQLiteKnowledgeStore>,
) -> ServerBuilder {
    let kstore = store;
    builder
        .tool(
            ToolDefinition {
                name: "inspect_entity".into(),
                description: "Return the full picture for a named entity: object, events, relations, evidence, and mentions".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Entity name (e.g. 赵云)"},
                        "doc": {"type": "string", "description": "Optional document title filter"}
                    },
                    "required": ["name"]
                }),
            },
            Arc::new(InspectEntityHandler { store: kstore.clone() }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "timeline".into(),
                description: "Get a chronological timeline of events for an entity".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "entity": {"type": "string", "description": "Entity name (e.g. 赵云)"},
                        "doc": {"type": "string", "description": "Optional document title filter"}
                    },
                    "required": ["entity"]
                }),
            },
            Arc::new(TimelineHandler { store: kstore.clone() }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "relation_graph".into(),
                description: "Traverse the relation graph outward from an entity (BFS)".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "entity": {"type": "string", "description": "Starting entity name"},
                        "doc": {"type": "string", "description": "Optional document title filter"},
                        "depth": {"type": "integer", "default": 2, "description": "Traversal depth (1-5)"}
                    },
                    "required": ["entity"]
                }),
            },
            Arc::new(RelationGraphHandler { store: kstore.clone() }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "evidence".into(),
                description: "Search evidence snippets by keyword across all compiled documents".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query"},
                        "doc": {"type": "string", "description": "Optional document title filter"},
                        "limit": {"type": "integer", "default": 20, "description": "Max results"}
                    },
                    "required": ["query"]
                }),
            },
            Arc::new(EvidenceHandler { store: kstore }),
        )
        .await
}
