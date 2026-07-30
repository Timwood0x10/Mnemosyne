//! MCP tool handlers for the general knowledge model (dev_guide §5).
//!
//! Registers four read-only query tools and one correction tool — `inspect_entity`,
//! `timeline`, `relation_graph`, `evidence`, and `correct_relation`.
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
use crate::fact_store::SqliteFactStore;
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{KnowledgeEdge, SQLiteKnowledgeStore};
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
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::Internal(format!("serialize result: {e}")))?;
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
        // Clamp to a sane upper bound so a malicious/huge `limit` can't exhaust
        // memory by loading the whole evidence table into one response.
        let limit = opt_usize(args, "limit", 20).min(200);
        let hits = self.store.search_evidence(&query, doc, limit).await?;
        json_ok(&hits)
    }
}

// ── correct_relation ──────────────────────────────────────────────────────

/// Correct a misattributed relation in the knowledge graph.
///
/// Finds edges matching (source_name, predicate) and replaces the target.
struct CorrectRelationHandler {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait::async_trait]
impl ToolHandler for CorrectRelationHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let source = req_str(args, "source")?;
        let predicate = req_str(args, "predicate")?;
        let old_target = req_str(args, "old_target")?;
        let new_target = req_str(args, "new_target")?;
        let doc = opt_str(args, "doc");

        // Resolve all three entities up front so the edge filter can match on
        // the full triple (source, predicate, old_target) rather than just the
        // predicate. The previous implementation resolved `old_target` but
        // never used it to filter, so ALL edges with the matching predicate
        // were reported as "changed" — even ones pointing at other entities.
        let src = match self.store.find_object_by_name(&source, None).await? {
            Some(s) => s,
            None => return Ok(err_result(format!("source `{source}` not found"))),
        };
        let target_entity = match self.store.find_object_by_name(&old_target, None).await? {
            Some(t) => t,
            None => return Ok(err_result(format!("old_target `{old_target}` not found"))),
        };
        let new_entity = match self.store.find_object_by_name(&new_target, None).await? {
            Some(n) => n,
            None => return Ok(err_result(format!("new_target `{new_target}` not found"))),
        };

        // Match OUTGOING edges from `source` to `old_target` with the given
        // predicate. `get_edges_touching` also returns incoming edges, so we
        // explicitly require `source_id == src.id` to avoid re-targeting edges
        // where `source` is the object of someone else's relation.
        let edges = self.store.get_edges_touching(src.id).await?;
        let matched: Vec<&KnowledgeEdge> = edges
            .iter()
            .filter(|e| {
                e.predicate == predicate && e.source_id == src.id && e.target_id == target_entity.id
            })
            .collect();

        if matched.is_empty() {
            return Ok(err_result(format!(
                "no `{predicate}` edge from `{source}` to `{old_target}` found"
            )));
        }

        // Persist the correction: re-target each matching edge in place. This
        // closes the loop that previously left the database unchanged while
        // reporting success to the client.
        let mut changed = 0usize;
        for e in &matched {
            self.store.update_edge_target(e.id, new_entity.id).await?;
            changed += 1;
        }

        let details = serde_json::json!({
            "source": source,
            "predicate": predicate,
            "changed": changed,
            "from_target": old_target,
            "from_id": target_entity.id,
            "to_target": new_target,
            "to_id": new_entity.id,
            "doc": doc,
        });

        json_ok(&details)
    }
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Register the general-knowledge MCP tools on `builder`.
///
/// `fact_store` provides durable fact reads for cognition snapshots, sharing
/// the same SQLite database written by the compilation pipeline.
/// Each tool wraps a [`KnowledgeStore`] method and speaks JSON-RPC 2.0 over
/// the existing [`ServerBuilder`] infrastructure.
pub async fn register_knowledge_tools(
    builder: ServerBuilder,
    store: Arc<SQLiteKnowledgeStore>,
    fact_store: Arc<SqliteFactStore>,
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
            Arc::new(EvidenceHandler { store: kstore.clone() }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "correct_relation".into(),
                description: "Correct a misattributed relation in the knowledge graph. Finds edges matching (source_name, predicate, old_target) and reports the correction needed.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "source": {"type": "string", "description": "Source entity name"},
                        "predicate": {"type": "string", "description": "Relation predicate to match"},
                        "old_target": {"type": "string", "description": "Current (incorrect) target name"},
                        "new_target": {"type": "string", "description": "Correct target name"},
                        "doc": {"type": "string", "description": "Optional document title filter"}
                    },
                    "required": ["source", "predicate", "old_target", "new_target"]
                }),
            },
            Arc::new(CorrectRelationHandler { store: kstore.clone() }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "cognitive_context".into(),
                description: "Return a structured cognitive snapshot for an entity: identity, preferences, goals, events, relationships, and timeline. Replaces the older inspect_entity for new cognition pipeline data.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Entity name (e.g. User, 赵云, Pierre)"},
                        "tenant_id": {"type": "string", "default": "default", "description": "Tenant scope for cognition entities"},
                        "user_id": {"type": "string", "description": "External user id when querying the User entity"}
                    },
                    "required": ["name"]
                }),
            },
            Arc::new(CognitiveContextHandler {
                store: kstore,
                fact_store,
            }),
        )
        .await
}

/// Handler for the `cognitive_context` tool.
///
/// Builds an EntitySnapshot from the knowledge store using the cognition
/// pipeline: Facts → StateEngine → Snapshot → CognitiveContext.
struct CognitiveContextHandler {
    store: Arc<SQLiteKnowledgeStore>,
    fact_store: Arc<SqliteFactStore>,
}

#[async_trait::async_trait]
impl ToolHandler for CognitiveContextHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        use crate::cognition::{FactStore as CognitionFactStore, StateEngine, build_snapshot};

        let name = req_str(args, "name")?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let user_id = args.get("user_id").and_then(Value::as_str).unwrap_or("");
        let store = &self.store;

        // 1. Resolve persisted knowledge entities. User identities are scoped
        // by tenant and external user id; legacy callers still resolve the
        // default User root through the compatible empty-id mapping.
        let (entity_id, entity_name, entity_type) = if name.eq_ignore_ascii_case("user") {
            let entity_id = self.fact_store.resolve_user(tenant_id, user_id)?;
            let entity_name = if user_id.is_empty() {
                "User".to_string()
            } else {
                format!("User:{user_id}")
            };
            (entity_id, entity_name, "User".to_string())
        } else {
            let object = store
                .find_object_by_name(&name, None)
                .await?
                .ok_or_else(|| Error::NotFound(format!("entity `{name}` not found")))?;
            (object.id, object.name, format!("{:?}", object.object_type))
        };

        // 2. Read Facts from the same SQLite database as the knowledge store.
        // This keeps entity resolution and cognition state on one durable path.
        let facts = self.fact_store.get_facts(entity_id)?;

        // 4. Build snapshot
        let state_engine = StateEngine::new();
        let snapshot = build_snapshot(entity_id, entity_name, entity_type, facts, &state_engine);

        // 5. Return as JSON
        let json = snapshot.format_json();
        json_ok(&json)
    }
}
