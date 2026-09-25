//! MCP tool for relationship-path tracing between two entities.
//!
//! `trace_path` runs a breadth-first search over the knowledge-graph edges to
//! find the shortest hop-by-hop path between two named entities — the
//! "how are these two connected?" query. It returns the ordered node names and
//! the relation predicate on each hop, plus the total path length and the
//! number of edges explored. A missing route within `max_depth` is a graceful
//! "no path" result rather than an error.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::knowledge::SQLiteKnowledgeStore;
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{KnowledgeEdge, KnowledgeObject};
use crate::mcp::server::ServerBuilder;
use crate::mcp::types::{ContentBlock, ToolCallResult, ToolDefinition, ToolHandler};

/// Default BFS depth cap when the caller omits `max_depth`.
const DEFAULT_MAX_DEPTH: usize = 5;

/// Build a JSON text result block.
fn json_block(value: &impl serde::Serialize, is_error: bool) -> Result<ToolCallResult> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| crate::error::Error::Internal(format!("serialize result: {e}")))?;
    Ok(ToolCallResult {
        content: vec![ContentBlock {
            block_type: "text".into(),
            text: Some(text),
            mime_type: Some("application/json".into()),
        }],
        is_error,
    })
}

/// The `trace_path` handler.
pub struct TracePathTool {
    store: Arc<SQLiteKnowledgeStore>,
}

impl TracePathTool {
    #[must_use]
    pub fn new(store: Arc<SQLiteKnowledgeStore>) -> Self {
        Self { store }
    }

    /// Resolve an object by name (global), returning its id.
    async fn resolve(&self, name: &str) -> Result<Option<KnowledgeObject>> {
        self.store.find_object_by_name(name, None).await
    }
}

#[async_trait::async_trait]
impl ToolHandler for TracePathTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let source = args.get("source").and_then(Value::as_str).unwrap_or("");
        let target = args.get("target").and_then(Value::as_str).unwrap_or("");
        let max_depth = args
            .get("max_depth")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_MAX_DEPTH as u64)
            .clamp(1, 20) as usize;

        if source.is_empty() || target.is_empty() {
            return json_block(
                &serde_json::json!({"found": false, "error": "`source` and `target` are required"}),
                true,
            );
        }
        let (Some(src), Some(tgt)) = (self.resolve(source).await?, self.resolve(target).await?)
        else {
            return json_block(
                &serde_json::json!({
                    "found": false,
                    "error": format!("entity not found: `{}`", if self.resolve(source).await?.is_some() { target } else { source })
                }),
                true,
            );
        };

        // Same entity → trivial zero-hop path.
        if src.id == tgt.id {
            return json_block(
                &serde_json::json!({
                    "found": true,
                    "source": source,
                    "target": target,
                    "path": [source],
                    "relations": [],
                    "length": 0,
                }),
                false,
            );
        }

        // BFS over undirected edges: `prev[neighbor] = (current, incoming_edge)`.
        // The source's own entry carries `None` as a sentinel; backtracking
        // stops at the source before ever reading it.
        let mut prev: HashMap<i64, (i64, Option<KnowledgeEdge>)> = HashMap::new();
        let mut queue: VecDeque<(i64, usize)> = VecDeque::new();
        queue.push_back((src.id, 0));
        prev.insert(src.id, (src.id, None));

        let mut explored = 0usize;
        let mut found = false;
        while let Some((cur, depth)) = queue.pop_front() {
            if cur == tgt.id {
                found = true;
                break;
            }
            if depth >= max_depth {
                continue;
            }
            for edge in self.store.get_edges_touching(cur).await? {
                explored += 1;
                let neighbor = if edge.source_id == cur {
                    edge.target_id
                } else {
                    edge.source_id
                };
                if let std::collections::hash_map::Entry::Vacant(e) = prev.entry(neighbor) {
                    e.insert((cur, Some(edge)));
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        if !found {
            return json_block(
                &serde_json::json!({
                    "found": false,
                    "source": source,
                    "target": target,
                    "error": format!("no path within depth {max_depth}"),
                    "explored": explored,
                }),
                false,
            );
        }

        // Reconstruct source → target: walk `prev` back from target.
        //
        // The BFS bookkeeping guarantees both lookups (a node enters `prev`
        // only when discovered from `src`, always with the edge that discovered
        // it, and the walk stops before the source's `None` sentinel). It is
        // still enforced as an error rather than an `expect`: this runs inside
        // a request handler, so a future break of that invariant must fail the
        // call, not panic the server for every other client.
        let mut node_ids = vec![tgt.id];
        let mut rels = Vec::new();
        let mut cur = tgt.id;
        while cur != src.id {
            let (parent, edge) = prev.get(&cur).cloned().ok_or_else(|| {
                Error::Internal(format!("path node {cur} is absent from the BFS tree"))
            })?;
            let edge = edge.ok_or_else(|| {
                Error::Internal(format!("path node {cur} has no discovering edge"))
            })?;
            // The predicate is read off the edge regardless of direction.
            rels.push(edge.predicate.clone());
            node_ids.push(parent);
            cur = parent;
        }
        node_ids.reverse();
        rels.reverse();

        // Map ids back to names for a readable path.
        let mut names = Vec::with_capacity(node_ids.len());
        for id in &node_ids {
            match self.store.get_object(*id).await? {
                Some(o) => names.push(o.name),
                None => names.push(format!("<missing:{id}>")),
            }
        }

        json_block(
            &serde_json::json!({
                "found": true,
                "source": source,
                "target": target,
                "path": names,
                "relations": rels,
                "length": rels.len(),
                "explored": explored,
            }),
            false,
        )
    }
}

/// Register the `trace_path` tool on `builder`.
pub async fn register_trace_path_tool(
    builder: ServerBuilder,
    store: Arc<SQLiteKnowledgeStore>,
) -> ServerBuilder {
    builder
        .tool(
            ToolDefinition {
                name: "trace_path".into(),
                description: "Find the shortest relationship path between two named entities (breadth-first over knowledge-graph edges). Returns the ordered entity names, the relation predicate on each hop, and the path length. Set `max_depth` to bound the search; a route beyond it returns a graceful 'no path' result.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "source": {"type": "string", "description": "Starting entity name"},
                        "target": {"type": "string", "description": "Destination entity name"},
                        "max_depth": {"type": "integer", "default": 5, "description": "Max BFS depth (capped at 20)"}
                    },
                    "required": ["source", "target"]
                }),
            },
            Arc::new(TracePathTool::new(store)),
        )
        .await
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::memory_export::{
        ExportBundle, ExportDocument, ExportObject, import_bundle,
    };

    /// Seed a small graph: A →(关系甲)→ B, B →(关系乙)→ C, plus an isolated D.
    async fn seed_store() -> Arc<SQLiteKnowledgeStore> {
        let store = Arc::new(
            SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("open in-memory"),
        );
        let bundle = ExportBundle {
            format: "lorescope-memory".into(),
            version: 1,
            exported_at: 0,
            documents: vec![ExportDocument {
                title: "关系图".into(),
                author: None,
                doc_type: Some("text".into()),
            }],
            objects: vec![
                ExportObject {
                    doc_title: "关系图".into(),
                    object_type: "person".into(),
                    name: "甲".into(),
                    properties: serde_json::json!({}),
                    confidence: 0.9,
                },
                ExportObject {
                    doc_title: "关系图".into(),
                    object_type: "person".into(),
                    name: "乙".into(),
                    properties: serde_json::json!({}),
                    confidence: 0.9,
                },
                ExportObject {
                    doc_title: "关系图".into(),
                    object_type: "person".into(),
                    name: "丙".into(),
                    properties: serde_json::json!({}),
                    confidence: 0.9,
                },
                ExportObject {
                    doc_title: "关系图".into(),
                    object_type: "person".into(),
                    name: "丁".into(),
                    properties: serde_json::json!({}),
                    confidence: 0.9,
                },
            ],
            edges: vec![
                crate::knowledge::memory_export::ExportEdge {
                    doc_title: "关系图".into(),
                    source_name: "甲".into(),
                    predicate: "关系甲".into(),
                    target_name: "乙".into(),
                    properties: serde_json::json!({}),
                    origin: "observed".into(),
                    confidence: 0.8,
                    valid_from: None,
                    valid_to: None,
                },
                crate::knowledge::memory_export::ExportEdge {
                    doc_title: "关系图".into(),
                    source_name: "乙".into(),
                    predicate: "关系乙".into(),
                    target_name: "丙".into(),
                    properties: serde_json::json!({}),
                    origin: "observed".into(),
                    confidence: 0.8,
                    valid_from: None,
                    valid_to: None,
                },
            ],
            evidence: vec![],
            evidence_links: vec![],
            world_entities: vec![],
            world_profiles: vec![],
            world_relations: vec![],
        };
        import_bundle(store.as_ref(), &bundle).await.expect("seed");
        store
    }

    /// Objective: Verify a two-hop path A→B→C is found with the expected
    /// ordered names and relation predicates.
    /// Invariants: found=true; path=[甲,乙,丙]; relations=[关系甲,关系乙].
    #[tokio::test]
    async fn finds_two_hop_path() {
        let store = seed_store().await;
        let handler = TracePathTool::new(store);
        let result = handler
            .call(&serde_json::json!({"source": "甲", "target": "丙"}))
            .await
            .expect("call");
        assert!(!result.is_error, "path should be found");
        let text = result.content[0].text.clone().unwrap_or_default();
        let json: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["found"], true, "found, got: {text}");
        assert_eq!(
            json["path"],
            serde_json::json!(["甲", "乙", "丙"]),
            "ordered path"
        );
        assert_eq!(
            json["relations"],
            serde_json::json!(["关系甲", "关系乙"]),
            "relation predicates per hop"
        );
        assert_eq!(json["length"], 2, "two hops");
    }

    /// Objective: Verify same-entity input yields a trivial zero-hop path.
    /// Invariants: found=true; path=[名]; length=0.
    #[tokio::test]
    async fn same_entity_trivial_path() {
        let store = seed_store().await;
        let handler = TracePathTool::new(store);
        let result = handler
            .call(&serde_json::json!({"source": "甲", "target": "甲"}))
            .await
            .expect("call");
        let text = result.content[0].text.clone().unwrap_or_default();
        let json: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["found"], true, "same entity is reachable");
        assert_eq!(json["length"], 0, "zero hops");
    }

    /// Objective: Verify an isolated node (no route) yields a graceful
    /// "no path" result within the depth cap — not an error.
    /// Invariants: found=false; error mentions "no path".
    #[tokio::test]
    async fn isolated_node_reports_no_path() {
        let store = seed_store().await;
        let handler = TracePathTool::new(store);
        let result = handler
            .call(&serde_json::json!({"source": "甲", "target": "丁"}))
            .await
            .expect("call");
        assert!(!result.is_error, "no-path is a graceful result");
        let text = result.content[0].text.clone().unwrap_or_default();
        let json: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["found"], false, "not found, got: {text}");
        assert!(
            json["error"].as_str().unwrap_or("").contains("no path"),
            "clear message, got: {text}"
        );
    }

    /// Objective: Verify an unknown entity name yields a graceful error.
    /// Invariants: found=false; error mentions "entity not found".
    #[tokio::test]
    async fn unknown_entity_reports_graceful_error() {
        let store = seed_store().await;
        let handler = TracePathTool::new(store);
        let result = handler
            .call(&serde_json::json!({"source": "甲", "target": "不存在"}))
            .await
            .expect("call");
        assert!(result.is_error, "unknown entity → error result");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("entity not found"),
            "clear message, got: {text}"
        );
    }

    /// Objective: Verify missing required args are rejected gracefully.
    /// Invariants: no source/target → error result.
    #[tokio::test]
    async fn missing_args_rejected() {
        let store = seed_store().await;
        let handler = TracePathTool::new(store);
        let result = handler.call(&serde_json::json!({})).await.expect("call");
        assert!(result.is_error, "missing args → error result");
    }
}
