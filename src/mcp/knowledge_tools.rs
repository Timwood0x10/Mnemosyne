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

use std::sync::{Arc, RwLock};

use serde_json::Value;

use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{EntityLinker, ExternalAlias, KnowledgeEdge, SQLiteKnowledgeStore};
use crate::mcp::server::ServerBuilder;
use crate::mcp::types::{ContentBlock, ToolCallResult, ToolDefinition, ToolHandler};

/// Shared, runtime-mutable entity linker.
///
/// Wrapped in `Arc<RwLock<...>>` so `inspect_entity` can read the current
/// cross-source links while `knowledge_attach` rebuilds the linker after
/// attaching a new source (external-knowledge-plan §D3, §E).
pub type SharedEntityLinker = Arc<RwLock<EntityLinker>>;

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
    /// Optional cross-source entity linker (external-knowledge-plan §D3).
    /// When attached, the requested `name` is resolved through the linker
    /// before querying the graph, and the response carries the external
    /// aliases that map to the resolved entity.
    linker: Option<SharedEntityLinker>,
}

#[async_trait::async_trait]
impl ToolHandler for InspectEntityHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let requested = req_str(args, "name")?;
        let doc = opt_str(args, "doc");
        // Optional source hint: when the caller knows the surface came from a
        // specific external source, scope the linker lookup so identical
        // surfaces that different sources map differently are disambiguated.
        let source_hint = opt_str(args, "source");

        // Phase D3: resolve the requested name through the EntityLinker first.
        // The linker returns the canonical name; if the surface is unknown OR
        // the linker is not attached, fall back to the requested name so the
        // tool keeps working for purely-local entities.
        let (canonical, external_aliases) = self.resolve_with_linker(&requested, source_hint);

        match self.store.inspect_entity(&canonical, doc).await? {
            Some(mut result) => {
                // Attach the cross-source aliases that resolved to this entity
                // so the response carries a full cross-source picture
                // (external-knowledge-plan §D: "返回跨来源画像").
                result.external_aliases = external_aliases;
                json_ok(&result)
            }
            None => {
                // If the canonical lookup missed, retry with the original
                // requested name as a last resort (the linker may have mapped
                // the surface to a canonical that isn't in the graph yet).
                if canonical != requested {
                    if let Some(mut result) = self.store.inspect_entity(&requested, doc).await? {
                        result.external_aliases = external_aliases;
                        return json_ok(&result);
                    }
                }
                Ok(err_result(format!("entity `{requested}` not found")))
            }
        }
    }
}

impl InspectEntityHandler {
    /// Resolve `requested` through the attached linker (if any) and collect
    /// the external aliases that map to the resolved canonical.
    ///
    /// Returns `(canonical_name, external_aliases)`. When no linker is
    /// attached or the surface is unknown, the canonical equals `requested`
    /// and the alias list is empty.
    fn resolve_with_linker(
        &self,
        requested: &str,
        source_hint: Option<&str>,
    ) -> (String, Vec<ExternalAlias>) {
        let Some(linker_arc) = &self.linker else {
            return (requested.to_string(), Vec::new());
        };
        // std::sync::RwLock read guard is fine here: no .await is held while
        // the guard is live, and the linker is a cheap HashMap lookup.
        let linker = linker_arc
            .read()
            .expect("entity linker lock poisoned (reader)");
        match linker.resolve(requested, source_hint) {
            Some(canonical) => {
                let aliases = linker
                    .provenance(canonical)
                    .iter()
                    .map(|link| ExternalAlias {
                        source: link.source.clone(),
                        external_name: link.external_name.clone(),
                    })
                    .collect();
                (canonical.to_string(), aliases)
            }
            None => (requested.to_string(), Vec::new()),
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
        // The `doc` param is passed through to entity resolution (NEW-K2): a
        // correction scoped to one document must not re-target a same-named
        // entity from another document. `find_document_by_title` is the
        // public trait API (the private `resolve_doc_id` helper is not
        // visible from the handler module).
        //
        // An explicitly named document that does NOT exist is an error, not a
        // silent downgrade to a global (doc_id=None) correction — the caller
        // asked to scope the fix to a document, and silently applying it
        // graph-wide would re-target same-named entities in other documents.
        let doc_id = match doc {
            Some(title) => match self.store.find_document_by_title(title).await? {
                Some(d) => Some(d.id),
                None => {
                    return Ok(err_result(format!("document `{title}` not found")));
                }
            },
            None => None,
        };
        let src = match self.store.find_object_by_name(&source, doc_id).await? {
            Some(s) => s,
            None => return Ok(err_result(format!("source `{source}` not found"))),
        };
        let target_entity = match self.store.find_object_by_name(&old_target, doc_id).await? {
            Some(t) => t,
            None => return Ok(err_result(format!("old_target `{old_target}` not found"))),
        };
        let new_entity = match self.store.find_object_by_name(&new_target, doc_id).await? {
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
            // Use the actual affected-row count from the store (NEW-M1): an
            // unknown edge id returns 0 rows and must not inflate `changed`.
            changed += self.store.update_edge_target(e.id, new_entity.id).await?;
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
/// `entity_linker` is the optional shared cross-source linker
/// (external-knowledge-plan §D3): when supplied, `inspect_entity` resolves
/// external surface names to unified graph nodes and decorates the response
/// with the matching external aliases.
/// Each tool wraps a [`KnowledgeStore`] method and speaks JSON-RPC 2.0 over
/// the existing [`ServerBuilder`] infrastructure.
pub async fn register_knowledge_tools(
    builder: ServerBuilder,
    store: Arc<SQLiteKnowledgeStore>,
    fact_store: Arc<SqliteFactStore>,
    entity_linker: Option<SharedEntityLinker>,
) -> ServerBuilder {
    let kstore = store;
    builder
        .tool(
            ToolDefinition {
                name: "inspect_entity".into(),
                description: "Return the full picture for a named entity: object, events, relations, evidence, mentions, and cross-source aliases. External surface names (e.g. 'John Smith') are resolved to the unified graph node via the entity linker when a source is attached.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Entity name or external surface (e.g. 赵云, John Smith)"},
                        "doc": {"type": "string", "description": "Optional document title filter"},
                        "source": {"type": "string", "description": "Optional external source hint to disambiguate identical surface names across sources (external-knowledge-plan §D)"}
                    },
                    "required": ["name"]
                }),
            },
            Arc::new(InspectEntityHandler {
                store: kstore.clone(),
                linker: entity_linker,
            }),
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
                description: "Correct a misattributed relation in the knowledge graph. Finds edges matching (source_name, predicate, old_target) and APPLIES the correction in place, re-targeting each matching edge to new_target and reporting how many were changed.".into(),
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

// ───────────────────────────────────────────────────────────────────────────
// Tests — inspect_entity × EntityLinker integration (external-knowledge-plan §D)
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::adapter::EntityLink;
    use crate::knowledge::{Document, KnowledgeObject, ObjectType};
    use serde_json::json;

    fn now_ts() -> i64 {
        chrono::Utc::now().timestamp()
    }

    /// Build a shared linker preloaded with the CRM ↔ novel alias scenario
    /// from the plan (§D4): "John Smith" / "J. Smith" → "Mr. Smith".
    fn linked() -> SharedEntityLinker {
        let linker = EntityLinker::from_links(vec![
            EntityLink {
                external_name: "John Smith".into(),
                canonical_name: "Mr. Smith".into(),
                source: "crm".into(),
            },
            EntityLink {
                external_name: "J. Smith".into(),
                canonical_name: "Mr. Smith".into(),
                source: "novel".into(),
            },
        ]);
        Arc::new(RwLock::new(linker))
    }

    /// Objective: Verify resolve_with_linker maps an external surface to the
    /// canonical graph name and collects every (source, surface) alias.
    /// Invariants: "John Smith" → "Mr. Smith"; aliases include both CRM and
    /// novel surfaces; an unknown surface falls back to the requested name
    /// with empty aliases.
    #[tokio::test]
    async fn resolve_with_linker_maps_surface_to_canonical_and_collects_aliases() {
        let store = Arc::new(
            crate::knowledge::SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("open"),
        );
        let handler = InspectEntityHandler {
            store,
            linker: Some(linked()),
        };

        let (canonical, aliases) = handler.resolve_with_linker("John Smith", None);
        assert_eq!(
            canonical, "Mr. Smith",
            "external surface resolves to canonical"
        );
        assert_eq!(
            aliases.len(),
            2,
            "both CRM and novel surfaces are listed as aliases"
        );
        let mut pairs: Vec<(String, String)> = aliases
            .iter()
            .map(|a| (a.source.clone(), a.external_name.clone()))
            .collect();
        pairs.sort_unstable();
        assert_eq!(
            pairs,
            vec![
                ("crm".into(), "John Smith".into()),
                ("novel".into(), "J. Smith".into()),
            ],
            "alias list carries source + external surface pairs"
        );
    }

    /// Objective: Verify a source hint scopes the lookup so identical surfaces
    /// that different sources map differently are disambiguated.
    /// Invariants: "John Smith" + source=crm → "Mr. Smith"; with no linker
    /// attached the surface is returned unchanged with no aliases.
    #[tokio::test]
    async fn source_hint_scopes_resolution_and_no_linker_falls_back() {
        let store = Arc::new(
            crate::knowledge::SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("open"),
        );
        let handler_with_linker = InspectEntityHandler {
            store: store.clone(),
            linker: Some(linked()),
        };
        let (canonical, _) = handler_with_linker.resolve_with_linker("John Smith", Some("crm"));
        assert_eq!(
            canonical, "Mr. Smith",
            "source-scoped lookup resolves correctly"
        );

        // No linker attached: surface is returned as-is, no aliases.
        let handler_no_linker = InspectEntityHandler {
            store,
            linker: None,
        };
        let (canonical, aliases) = handler_no_linker.resolve_with_linker("John Smith", None);
        assert_eq!(
            canonical, "John Smith",
            "without a linker the requested name is returned unchanged"
        );
        assert!(aliases.is_empty(), "no aliases without a linker");
    }

    /// Objective: Verify the full inspect_entity handler resolves an external
    /// surface ("John Smith") to the graph node ("Mr. Smith") end-to-end and
    /// decorates the response with cross-source aliases
    /// (external-knowledge-plan §D4: "John Smith" ↔ "Mr. Smith" 同一实体").
    /// Invariants: the returned object name is "Mr. Smith"; external_aliases
    /// lists both surfaces; an unknown surface still works via fallback.
    #[tokio::test]
    async fn inspect_entity_resolves_external_surface_to_graph_node() {
        let store = Arc::new(
            crate::knowledge::SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("open"),
        );
        // Seed the graph with the canonical "Mr. Smith" node.
        let did = store
            .create_document(&Document {
                id: 0,
                title: "crm-export".into(),
                author: None,
                doc_type: Some("novel".into()),
                created_at: now_ts(),
            })
            .await
            .expect("create doc");
        store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "Mr. Smith".into(),
                properties: json!({}),
                confidence: 0.9,
                created_at: now_ts(),
            })
            .await
            .expect("create Mr. Smith");

        let handler = InspectEntityHandler {
            store: store.clone(),
            linker: Some(linked()),
        };

        // Query with the EXTERNAL surface "John Smith" — must resolve to the
        // canonical "Mr. Smith" graph node.
        let args = serde_json::json!({"name": "John Smith"});
        let result = handler.call(&args).await.expect("handler succeeds");
        assert!(
            !result.is_error,
            "handler must not error on a resolvable surface"
        );
        let payload: serde_json::Value =
            serde_json::from_str(&result.content[0].text.clone().unwrap_or_default())
                .expect("result is JSON");
        assert_eq!(
            payload["object"]["name"].as_str(),
            Some("Mr. Smith"),
            "external surface resolved to the canonical graph node"
        );
        let aliases = payload["external_aliases"]
            .as_array()
            .expect("aliases array");
        assert_eq!(
            aliases.len(),
            2,
            "both cross-source surfaces appear in the response"
        );
    }

    /// Objective: Verify inspect_entity returns an error result (not a panic)
    /// for a surface that the linker cannot resolve AND that is absent from
    /// the graph.
    /// Invariants: is_error true; message names the missing entity.
    #[tokio::test]
    async fn inspect_entity_unknown_surface_returns_error_not_panic() {
        let store = Arc::new(
            crate::knowledge::SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("open"),
        );
        let handler = InspectEntityHandler {
            store,
            linker: Some(linked()),
        };
        let args = serde_json::json!({"name": "totally-unknown-person"});
        let result = handler.call(&args).await.expect("handler does not error");
        assert!(result.is_error, "unknown entity yields an error result");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("totally-unknown-person"),
            "error message names the missing entity"
        );
    }

    /// Objective: Verify correct_relation rejects an explicitly named but
    /// nonexistent `doc` instead of silently downgrading to a global
    /// (doc_id=None) correction that would re-target same-named entities in
    /// other documents.
    /// Invariants: with a graph containing the triple and a bogus doc title,
    /// the handler returns an error result naming the document, and the edge
    /// is left unchanged (not re-targeted graph-wide).
    #[tokio::test]
    async fn correct_relation_unknown_doc_is_rejected_not_global() {
        let store = Arc::new(
            crate::knowledge::SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("open"),
        );
        let did = store
            .create_document(&Document {
                id: 0,
                title: "三国演义".into(),
                author: None,
                doc_type: Some("novel".into()),
                created_at: now_ts(),
            })
            .await
            .expect("create doc");
        let zhaoyun = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "赵云".into(),
                properties: json!({}),
                confidence: 0.9,
                created_at: now_ts(),
            })
            .await
            .expect("create 赵云");
        let liubei = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "刘备".into(),
                properties: json!({}),
                confidence: 0.9,
                created_at: now_ts(),
            })
            .await
            .expect("create 刘备");
        // The 关羽 object is not referenced by assertions (the correction is
        // rejected before any edge is re-targeted), but its creation keeps the
        // graph fully seeded for the "would have applied globally" scenario.
        let _guanyu = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "关羽".into(),
                properties: json!({}),
                confidence: 0.9,
                created_at: now_ts(),
            })
            .await
            .expect("create 关羽");
        store
            .create_edge(&crate::knowledge::KnowledgeEdge {
                id: 0,
                source_id: zhaoyun,
                target_id: liubei,
                predicate: "serves".into(),
                properties: json!({}),
                origin: crate::knowledge::Origin::Observed,
                confidence: 0.8,
                valid_from: None,
                valid_to: None,
                created_at: now_ts(),
            })
            .await
            .expect("create edge");

        let handler = CorrectRelationHandler {
            store: store.clone(),
        };
        let args = serde_json::json!({
            "source": "赵云",
            "predicate": "serves",
            "old_target": "刘备",
            "new_target": "关羽",
            "doc": "不存在的书"
        });
        let result = handler.call(&args).await.expect("handler does not error");
        assert!(
            result.is_error,
            "unknown doc must be rejected, not applied globally"
        );
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("不存在的书"),
            "error names the missing document, got: {text}"
        );

        // The edge must be untouched (no graph-wide re-target).
        let edges = store.get_edges_touching(zhaoyun).await.expect("get edges");
        assert_eq!(edges.len(), 1, "edge count unchanged");
        assert_eq!(
            edges[0].target_id, liubei,
            "edge still points at the original target"
        );
    }
}
