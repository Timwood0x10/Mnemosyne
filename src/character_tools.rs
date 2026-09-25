//! V1 `character_*` tool handlers kept on the binary side.

use std::path::PathBuf;
use std::sync::Arc;

use mnemosyne::character::{CharacterStore, SQLiteCharacterStore, traverse_character_network};
use mnemosyne::error::Error;
use mnemosyne::ingest::IngestionPipeline;
use mnemosyne::mcp::types::{ToolCallResult, ToolHandler};

/// Tool: search character knowledge graph (`character_search`).
pub(crate) struct CharacterSearchTool {
    pub(crate) store: Arc<SQLiteCharacterStore>,
}

#[async_trait::async_trait]
impl ToolHandler for CharacterSearchTool {
    async fn call(&self, args: &serde_json::Value) -> Result<ToolCallResult, Error> {
        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `query`".into()))?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("novels");
        let novel = args.get("novel").and_then(serde_json::Value::as_str);
        // Default 10 to match the schema, and clamp to a sane upper bound
        // (NEW-M4): the handler previously defaulted to 50 while the schema
        // documented 10, and accepted unbounded limits.
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(10)
            .min(200) as usize;
        let include_events = args
            .get("include_events")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let include_relations = args
            .get("include_relations")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let characters = self
            .store
            .search_characters(query, tenant_id, novel, limit)
            .await?;

        let mut result_list = Vec::with_capacity(characters.len());
        for c in characters {
            let mut entry = serde_json::json!({
                "id": c.id,
                "name": c.name,
                "novel": c.novel,
                "aliases": c.aliases,
                "clothing": c.clothing,
                "personality": c.personality,
                "description": c.description,
                "importance": c.importance,
            });
            if include_events {
                let events = self
                    .store
                    .get_character_events(c.name.as_str(), tenant_id, novel)
                    .await?;
                entry["events"] = serde_json::to_value(events)?;
            }
            if include_relations {
                let relations = self
                    .store
                    .get_relations_for_character(c.name.as_str(), tenant_id, novel)
                    .await?;
                entry["relations"] = serde_json::to_value(relations)?;
            }
            result_list.push(entry);
        }

        let stats = serde_json::json!({
            "total_characters": self.store.count_characters(tenant_id, novel).await?,
            "total_events": self.store.count_events(tenant_id, novel).await?,
            "total_relations": self.store.count_relations(tenant_id, novel).await?,
        });

        let payload = serde_json::json!({
            "results": result_list,
            "stats": stats,
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Tool: traverse character knowledge network (`character_network`).
pub(crate) struct CharacterNetworkTool {
    pub(crate) store: Arc<SQLiteCharacterStore>,
}

#[async_trait::async_trait]
impl ToolHandler for CharacterNetworkTool {
    async fn call(&self, args: &serde_json::Value) -> Result<ToolCallResult, Error> {
        let name = args
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `name`".into()))?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("novels");
        let novel = args.get("novel").and_then(serde_json::Value::as_str);
        // Clamp depth to the documented 1-5 range. The previous `.min(5)`
        // allowed depth=0, which violates the tool contract and returns an
        // empty graph (BFS with depth 0 visits only the root).
        let depth = args
            .get("depth")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(2)
            .clamp(1, 5) as usize;

        let node =
            traverse_character_network(self.store.as_ref(), name, tenant_id, novel, depth).await?;

        let payload = serde_json::to_value(&node)?;
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Tool: distill the character knowledge graph from corpus text files
/// (`character_ingest`).
///
/// Runs the full ingestion pipeline over the four classical Chinese novels,
/// extracting characters, events, descriptions, and relationships into the
/// character store. This is a heavy operation (30-60s on full corpus).
pub(crate) struct CharacterIngestTool {
    pub(crate) store: Arc<SQLiteCharacterStore>,
}

#[async_trait::async_trait]
impl ToolHandler for CharacterIngestTool {
    async fn call(&self, args: &serde_json::Value) -> Result<ToolCallResult, Error> {
        let corpus_dir = args
            .get("corpus_dir")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("corpus");

        // Path sandbox: same policy as knowledge_attach — reject absolute
        // paths and `..` escapes so a remote client cannot point the ingest
        // at an arbitrary host directory.
        if PathBuf::from(corpus_dir).is_absolute()
            || corpus_dir.split('/').any(|seg| seg == "..")
            || corpus_dir.split('\\').any(|seg| seg == "..")
        {
            return Ok(ToolCallResult::text(
                serde_json::json!({
                    "status": "rejected",
                    "error": format!("corpus_dir must be a relative path under the workspace; rejected: {corpus_dir}"),
                })
                .to_string(),
            ));
        }

        let pipeline = IngestionPipeline::new(self.store.clone(), corpus_dir);
        let stats = pipeline.run().await?;

        let payload = serde_json::json!({
            "status": "completed",
            "characters": stats.characters,
            "events": stats.events,
            "relations": stats.relations,
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Tool: export the 3D character relationship graph as structured JSON
/// (`character_graph`).
///
/// Returns nodes (characters with appearance/personality/action attributes)
/// and edges (relations with dimensional scores) for frontend visualization.
/// This is the "立体人物关系网络": character → events → related characters,
/// with multi-dimensional edge weights.
pub(crate) struct CharacterGraphTool {
    pub(crate) store: Arc<SQLiteCharacterStore>,
}

#[async_trait::async_trait]
impl ToolHandler for CharacterGraphTool {
    async fn call(&self, args: &serde_json::Value) -> Result<ToolCallResult, Error> {
        let tenant_id = args
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("novels");
        let novel = args.get("novel").and_then(serde_json::Value::as_str);
        // Clamp max_nodes: an unbounded value (or a huge one like 1,000,000)
        // makes the engine materialize the entire character table via the
        // `LIKE '%%'` full scan plus a per-character relation query — the same
        // unbounded-resource hazard the other tools cap at 200.
        let max_nodes = args
            .get("max_nodes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(200)
            .min(200) as usize;

        // Retrieve characters (empty query matches all via LIKE '%%')
        let characters = self
            .store
            .search_characters("", tenant_id, novel, max_nodes)
            .await?;

        // Build node list with multi-dimensional attributes:
        //   - clothing  → 外貌 (appearance)
        //   - personality → 心理/性格 (psychology/character)
        //   - description → composite summary
        let nodes: Vec<serde_json::Value> = characters
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.name,
                    "label": c.name,
                    "novel": c.novel,
                    "aliases": c.aliases,
                    "dimensions": {
                        "appearance": c.clothing,
                        "personality": c.personality,
                        "description": c.description,
                    },
                    "importance": c.importance,
                })
            })
            .collect();

        // Collect edges (deduplicated bidirectional relations).
        //
        // Each edge surfaces three independent dimensional scores from the
        // relation's `metadata` bag, so frontend visualizations can render
        // *why* a relation is strong rather than only the combined weight:
        //   - co_occurrence_score : raw frequency / 15 (how often together)
        //   - event_coupling_score : shared events / total events (semantic)
        //   - relation_type_score : 1.0 for typed, 0.3 for generic "关联"
        // `weight` is the weighted combination (0.5/0.3/0.2) persisted as
        // `importance` by the ingestion pipeline.
        let mut edges: Vec<serde_json::Value> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for c in &characters {
            let rels = self
                .store
                .get_relations_for_character(c.name.as_str(), tenant_id, novel)
                .await?;
            for r in rels {
                // Normalize edge key so (A,B) and (B,A) are the same edge
                let key = if r.source_character <= r.target_character {
                    format!("{}|{}", r.source_character, r.target_character)
                } else {
                    format!("{}|{}", r.target_character, r.source_character)
                };
                if seen.insert(key) {
                    let md = &r.metadata.entries;
                    let get_f64 =
                        |k: &str| -> f64 { md.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) };
                    let get_u64 =
                        |k: &str| -> u64 { md.get(k).and_then(|v| v.as_u64()).unwrap_or(0) };
                    edges.push(serde_json::json!({
                        "source": r.source_character,
                        "target": r.target_character,
                        "relation_type": r.relation_type,
                        "weight": r.importance,
                        "description": r.description,
                        "chapter": r.chapter,
                        "dimensions": {
                            "co_occurrence_score": get_f64("co_occurrence_score"),
                            "event_coupling_score": get_f64("event_coupling_score"),
                            "relation_type_score": get_f64("relation_type_score"),
                            "co_occurrence_count": get_u64("co_occurrence_count"),
                            "shared_event_count": get_u64("shared_event_count"),
                            "detected_at_chapter": r.chapter,
                        },
                    }));
                }
            }
        }

        let payload = serde_json::json!({
            "nodes": nodes,
            "edges": edges,
            "stats": {
                "total_characters": self.store.count_characters(tenant_id, novel).await?,
                "total_events": self.store.count_events(tenant_id, novel).await?,
                "total_relations": self.store.count_relations(tenant_id, novel).await?,
                "visible_nodes": nodes.len(),
                "visible_edges": edges.len(),
            },
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}
