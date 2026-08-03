//! MCP tool for structured graph search.
//!
//! `search_graph` lets an agent query entities by type, name substring, or
//! attribute — a single structured call instead of many name lookups (the
//! "query once, not crawl" pattern). Each result carries the entity's
//! document, confidence, and how many relations touch it, so the agent can
//! pick the right anchor without extra round-trips.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::error::Result;
use crate::knowledge::SQLiteKnowledgeStore;
use crate::knowledge::store::KnowledgeStore;
use crate::mcp::server::ServerBuilder;
use crate::mcp::types::{ContentBlock, ToolCallResult, ToolDefinition, ToolHandler};

/// Default result cap when the caller omits `limit`.
const DEFAULT_LIMIT: usize = 20;

/// The `search_graph` handler.
pub struct GraphSearchTool {
    store: Arc<SQLiteKnowledgeStore>,
}

impl GraphSearchTool {
    #[must_use]
    pub fn new(store: Arc<SQLiteKnowledgeStore>) -> Self {
        Self { store }
    }
}

/// Resolve a document title → id map once per call, for O(1) title lookups.
async fn doc_title_by_id(store: &dyn KnowledgeStore) -> Result<HashMap<i64, String>> {
    let mut map = HashMap::new();
    for doc in store.list_documents().await? {
        map.insert(doc.id, doc.title);
    }
    Ok(map)
}

/// Collect the attribute keys whose value contains `needle` (case-insensitive
/// on the serialized properties). Empty when no attribute filter is given.
fn matched_attributes(properties: &Value, attribute: Option<&str>) -> Vec<String> {
    let Some(needle) = attribute else {
        return Vec::new();
    };
    let needle_lower = needle.to_lowercase();
    let mut out = Vec::new();
    if let Some(obj) = properties.as_object() {
        for (k, v) in obj {
            if k.to_lowercase().contains(&needle_lower)
                || v.to_string().to_lowercase().contains(&needle_lower)
            {
                out.push(k.clone());
            }
        }
    }
    out
}

#[async_trait::async_trait]
impl ToolHandler for GraphSearchTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let name_contains = args.get("query").and_then(Value::as_str);
        let object_type = args.get("object_type").and_then(Value::as_str);
        let attribute = args.get("attribute").and_then(Value::as_str);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_LIMIT as u64)
            .min(200) as usize;

        // Resolve an optional document scope by title.
        let doc_id = match args.get("doc_title").and_then(Value::as_str) {
            Some(title) => self
                .store
                .find_document_by_title(title)
                .await?
                .map(|d| d.id),
            None => None,
        };

        let objects = self
            .store
            .search_objects(name_contains, object_type, attribute, doc_id, limit)
            .await?;
        let titles = doc_title_by_id(self.store.as_ref()).await?;

        let mut results = Vec::with_capacity(objects.len());
        for obj in &objects {
            let edge_count = self.store.get_edges_touching(obj.id).await?.len();
            let attrs = matched_attributes(&obj.properties, attribute);
            results.push(serde_json::json!({
                "name": obj.name,
                "object_type": obj.object_type.as_str(),
                "doc_title": titles.get(&obj.doc_id).cloned().unwrap_or_default(),
                "confidence": obj.confidence,
                "edge_count": edge_count,
                "matched_attributes": attrs,
            }));
        }

        let text = serde_json::to_string_pretty(&serde_json::json!({
            "total": results.len(),
            "results": results,
        }))
        .map_err(|e| crate::error::Error::Internal(format!("serialize results: {e}")))?;

        Ok(ToolCallResult {
            content: vec![ContentBlock {
                block_type: "text".into(),
                text: Some(text),
                mime_type: Some("application/json".into()),
            }],
            is_error: false,
        })
    }
}

/// Register the `search_graph` tool on `builder`.
pub async fn register_graph_search_tool(
    builder: ServerBuilder,
    store: Arc<SQLiteKnowledgeStore>,
) -> ServerBuilder {
    builder
        .tool(
            ToolDefinition {
                name: "search_graph".into(),
                description: "Structured knowledge-graph search: find entities by name substring (`query`), object type (`object_type`), or attribute value (`attribute`), optionally scoped to one document (`doc_title`). Returns each match with its document, confidence, relation count, and matched attributes — one call instead of many entity lookups.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Substring to match against entity names"},
                        "object_type": {"type": "string", "enum": ["person", "event", "place", "organization", "concept"], "description": "Filter by entity type"},
                        "attribute": {"type": "string", "description": "Filter to entities whose properties contain this value"},
                        "doc_title": {"type": "string", "description": "Scope the search to one document by title"},
                        "limit": {"type": "integer", "default": 20, "description": "Max results (capped at 200)"}
                    }
                }),
            },
            Arc::new(GraphSearchTool::new(store)),
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

    async fn seed_store() -> Arc<SQLiteKnowledgeStore> {
        let store = Arc::new(
            SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("open in-memory"),
        );
        // Use the import path to seed a small graph (also exercises import).
        let bundle = ExportBundle {
            format: "lorescope-memory".into(),
            version: 1,
            exported_at: 0,
            documents: vec![ExportDocument {
                title: "人物志".into(),
                author: None,
                doc_type: Some("text".into()),
            }],
            objects: vec![
                ExportObject {
                    doc_title: "人物志".into(),
                    object_type: "person".into(),
                    name: "张三".into(),
                    properties: serde_json::json!({"偏好": "围棋", "职业": "棋手"}),
                    confidence: 0.9,
                },
                ExportObject {
                    doc_title: "人物志".into(),
                    object_type: "person".into(),
                    name: "李四".into(),
                    properties: serde_json::json!({"偏好": "象棋"}),
                    confidence: 0.8,
                },
                ExportObject {
                    doc_title: "人物志".into(),
                    object_type: "place".into(),
                    name: "江南".into(),
                    properties: serde_json::json!({}),
                    confidence: 0.7,
                },
            ],
            edges: vec![],
            evidence: vec![],
            evidence_links: vec![],
            world_entities: vec![],
            world_profiles: vec![],
            world_relations: vec![],
        };
        import_bundle(store.as_ref(), &bundle).await.expect("seed");
        store
    }

    /// Objective: Verify a type-filtered search returns only matching types.
    /// Invariants: object_type=person → 2 results; place → 1.
    #[tokio::test]
    async fn search_by_type() {
        let store = seed_store().await;
        let handler = GraphSearchTool::new(store);
        let result = handler
            .call(&serde_json::json!({"object_type": "person"}))
            .await
            .expect("call");
        let text = result.content[0].text.clone().unwrap_or_default();
        let json: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["total"], 2, "two persons, got: {text}");
        let names: Vec<&str> = json["results"]
            .as_array()
            .map(|a| a.iter().filter_map(|r| r["name"].as_str()).collect())
            .unwrap_or_default();
        assert!(names.contains(&"张三") && names.contains(&"李四"));
    }

    /// Objective: Verify a name-substring query narrows results.
    /// Invariants: query=张 → only 张三.
    #[tokio::test]
    async fn search_by_name_substring() {
        let store = seed_store().await;
        let handler = GraphSearchTool::new(store);
        let result = handler
            .call(&serde_json::json!({"query": "张"}))
            .await
            .expect("call");
        let text = result.content[0].text.clone().unwrap_or_default();
        let json: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["total"], 1, "one match, got: {text}");
        assert_eq!(json["results"][0]["name"], "张三");
    }

    /// Objective: Verify an attribute filter matches on property values and
    /// reports the matched attribute key.
    /// Invariants: attribute=棋 → 张三 (围棋) and 李四 (象棋), each with a
    /// matched attribute.
    #[tokio::test]
    async fn search_by_attribute() {
        let store = seed_store().await;
        let handler = GraphSearchTool::new(store);
        let result = handler
            .call(&serde_json::json!({"attribute": "棋"}))
            .await
            .expect("call");
        let text = result.content[0].text.clone().unwrap_or_default();
        let json: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["total"], 2, "two chess-lovers, got: {text}");
        for r in json["results"].as_array().expect("results") {
            let attrs = r["matched_attributes"].as_array().expect("attrs");
            assert!(!attrs.is_empty(), "matched attribute reported");
        }
    }

    /// Objective: Verify a combined query + type + doc_title narrows to the
    /// expected single entity, and that the doc_title is resolved.
    /// Invariants: query=李 + person + doc=人物志 → 李四 with doc_title set.
    #[tokio::test]
    async fn search_combined_filters() {
        let store = seed_store().await;
        let handler = GraphSearchTool::new(store);
        let result = handler
            .call(&serde_json::json!({
                "query": "李",
                "object_type": "person",
                "doc_title": "人物志"
            }))
            .await
            .expect("call");
        let text = result.content[0].text.clone().unwrap_or_default();
        let json: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["total"], 1, "one match, got: {text}");
        assert_eq!(json["results"][0]["name"], "李四");
        assert_eq!(json["results"][0]["doc_title"], "人物志");
    }

    /// Objective: Verify the limit is honored.
    /// Invariants: limit=1 on all persons → exactly one result.
    #[tokio::test]
    async fn search_respects_limit() {
        let store = seed_store().await;
        let handler = GraphSearchTool::new(store);
        let result = handler
            .call(&serde_json::json!({"object_type": "person", "limit": 1}))
            .await
            .expect("call");
        let text = result.content[0].text.clone().unwrap_or_default();
        let json: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["total"], 1, "limit honored, got: {text}");
    }
}
