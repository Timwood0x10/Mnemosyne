//! Main entry point for the Cognitive Memory MCP Server.
//!
//! Wires together the configuration, store, embedder, distiller, retrieval
//! engine, and MCP server, then registers the `memory_*` and `character_*`
//! MCP tools (`lore_scope`, `memory_compile`, `memory_search`,
//! `memory_store`, `memory_feedback`, `memory_stats`, `character_search`,
//! `character_network`, `character_ingest`, `character_graph`) and runs the
//! protocol loop over stdio.

use std::sync::Arc;

use anyhow::{Context, Result as AnyhowResult};
use clap::Parser;
use serde_json::Value;
use tracing_subscriber::EnvFilter;

use lore_scope::character::{CharacterStore, SQLiteCharacterStore, traverse_character_network};
use lore_scope::config::{CliArgs, Command, EmbeddingProvider};
use lore_scope::decay::{DecayConfig, run_decay_loop};
use lore_scope::distiller::{DistillationConfig, Distiller, PipelineDistiller};
#[cfg(feature = "remote-embed")]
use lore_scope::embed::RemoteEmbedder;
use lore_scope::embed::{EmbeddingService, NullEmbedder};
use lore_scope::error::Error;
use lore_scope::fact_store::SqliteFactStore;
use lore_scope::ingest::IngestionPipeline;
use lore_scope::knowledge::store::KnowledgeStore;
use lore_scope::knowledge::{Migrator, SQLiteKnowledgeStore};
use lore_scope::mcp::context_aware::{ContextCheckTool, context_check_definition};
use lore_scope::mcp::decay_tool::{MemoryDecayTool, memory_decay_definition};
use lore_scope::mcp::key_events_tool::{KeyEventsTool, key_events_definition};
use lore_scope::mcp::memory_compile::{MemoryCompileTool, memory_compile_definition};
use lore_scope::mcp::persona_check_tool::{PersonaCheckTool, persona_check_definition};
use lore_scope::mcp::persona_inject_tool::{PersonaInjectTool, persona_inject_definition};
use lore_scope::mcp::register_external_knowledge_tools;
use lore_scope::mcp::register_generalize_tool;
use lore_scope::mcp::register_graph_search_tool;
use lore_scope::mcp::register_knowledge_tools;
use lore_scope::mcp::register_memory_transfer_tools;
use lore_scope::mcp::register_trace_path_tool;
use lore_scope::mcp::relationship_tool::{
    PersonaTimelineTool, RelationshipQueryTool, RelationshipUpdateTool,
    persona_timeline_definition, relationship_query_definition, relationship_update_definition,
};
use lore_scope::mcp::serve_http_addr;
use lore_scope::mcp::story_bridge_tool::{StoryBridgeTool, story_bridge_definition};
use lore_scope::mcp::types::{Implementation, ToolCallResult, ToolDefinition, ToolHandler};
use lore_scope::mcp::{MCPServer, ServerBuilder, StdioTransport};
use lore_scope::retrieval::RetrievalEngine;
use lore_scope::store::{ExperienceRepository, SQLiteVecStore};
use lore_scope::types::{Experience, MemoryType, Message};

/// Tool: distill memories from a conversation (`lore_scope`).
struct MemoryDistillTool {
    distiller: Arc<PipelineDistiller>,
}

#[async_trait::async_trait]
impl ToolHandler for MemoryDistillTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let conv_id = args
            .get("conversation_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `conversation_id`".into()))?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let user_id = args.get("user_id").and_then(Value::as_str).unwrap_or("");
        let messages_raw = args
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidInput("missing `messages` array".into()))?;
        let messages = parse_messages(messages_raw)?;
        let memories = self
            .distiller
            .distill(conv_id, &messages, tenant_id, user_id)
            .await?;
        let metrics = self.distiller.metrics_ref().snapshot();
        let payload = serde_json::json!({
            "memories": memories,
            "metrics": metrics,
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Tool: search memories via the configured retrieval mode (`memory_search`).
struct MemorySearchTool {
    engine: Arc<RetrievalEngine>,
}

#[async_trait::async_trait]
impl ToolHandler for MemorySearchTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `query`".into()))?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        // Clamp the requested limit (NEW-M3): an unbounded value (e.g.
        // 1,000,000) would make the engine materialize the entire memory
        // table in one response. 200 mirrors the evidence tool's cap.
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .min(200) as usize;
        let memory_type_filter = args
            .get("memory_type")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<MemoryType>().ok());

        let results = self
            .engine
            .search(query, tenant_id, limit, memory_type_filter)
            .await?;
        let payload = serde_json::json!({ "results": results });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Tool: manually store a memory (`memory_store`).
struct MemoryStoreTool {
    store: Arc<dyn ExperienceRepository>,
}

#[async_trait::async_trait]
impl ToolHandler for MemoryStoreTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `content`".into()))?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let memory_type = args
            .get("memory_type")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<MemoryType>().ok())
            .unwrap_or(MemoryType::Knowledge);
        let confidence = args
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.5);

        let mut exp = Experience::new(tenant_id, memory_type, content, confidence);
        exp.source = "manual".to_string();
        self.store.create(&exp).await?;
        Ok(ToolCallResult::text(format!("stored memory `{}`", exp.id)))
    }
}

/// Tool: record agent feedback on a memory (`memory_feedback`).
///
/// Persists the feedback by adjusting the memory's importance (confidence)
/// and counting useful/not-useful votes in its metadata. This is the
/// self-evolution loop that `CODE_REVIEW_FINDINGS.md` H3 wanted: feedback is
/// no longer a log-only stub — it changes what the model will surface later.
struct MemoryFeedbackTool {
    store: Arc<dyn ExperienceRepository>,
}

/// Net confidence adjustment applied per useful / not-useful vote.
const FEEDBACK_CONFIDENCE_DELTA: f64 = 0.1;

#[async_trait::async_trait]
impl ToolHandler for MemoryFeedbackTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let memory_id = args
            .get("memory_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `memory_id`".into()))?;
        let useful = args.get("useful").and_then(Value::as_bool).unwrap_or(true);

        let Some(mut exp) = self.store.get(memory_id).await? else {
            return Ok(ToolCallResult::text(format!(
                "memory `{memory_id}` not found; feedback not applied"
            )));
        };

        // Apply the vote: adjust importance + tally in metadata (self-evolve).
        let votes = exp.apply_feedback(useful, FEEDBACK_CONFIDENCE_DELTA);
        self.store.update(&exp).await?;
        tracing::info!(
            memory_id = %memory_id,
            useful = %useful,
            votes = %votes,
            confidence = %exp.confidence,
            "memory feedback persisted"
        );
        Ok(ToolCallResult::text(format!(
            "feedback applied for memory `{memory_id}` (useful={useful}, votes={votes}, confidence={:.2})",
            exp.confidence
        )))
    }
}

/// Tool: aggregate memory health for a tenant (`memory_stats`).
///
/// Reports both the distilled-memory store (by type) and the knowledge-graph
/// health (documents / entities / relations / evidence), so an agent can
/// gauge at a glance how complete the persona's memory is.
struct MemoryStatsTool {
    store: Arc<dyn ExperienceRepository>,
    kgraph: Arc<SQLiteKnowledgeStore>,
}

#[async_trait::async_trait]
impl ToolHandler for MemoryStatsTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let counts = self.store.counts_by_type(tenant_id).await?;
        let total = self.store.count_for_tenant(tenant_id).await?;
        let mut by_type = serde_json::Map::new();
        for (mt, count) in counts {
            by_type.insert(mt.as_str().to_string(), Value::from(count));
        }
        let graph = self.kgraph.graph_counts().await?;
        let payload = serde_json::json!({
            "total_memories": total,
            "by_type": by_type,
            "knowledge_graph": {
                "documents": graph.documents,
                "entities": graph.objects,
                "relations": graph.edges,
                "evidence": graph.evidence,
            },
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Parse the `messages` array from a `lore_scope` tool call.
fn parse_messages(arr: &[Value]) -> Result<Vec<Message>, Error> {
    let mut out = Vec::with_capacity(arr.len());
    for raw in arr {
        let obj = raw
            .as_object()
            .ok_or_else(|| Error::InvalidInput("each message must be an object".into()))?;
        let role = obj
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("message missing `role`".into()))?;
        let content = obj
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("message missing `content`".into()))?;
        let mut msg = Message::new(role, content);
        if let Some(t) = obj.get("turn_id").and_then(Value::as_str) {
            msg.turn_id = Some(t.to_string());
        }
        if let Some(t) = obj.get("tool_call_id").and_then(Value::as_str) {
            msg.tool_call_id = Some(t.to_string());
        }
        out.push(msg);
    }
    Ok(out)
}

/// Build the embedder based on the configured `embedding_provider`.
fn build_embedder(cfg: &lore_scope::config::Config) -> AnyhowResult<Arc<dyn EmbeddingService>> {
    match cfg.embedding_provider {
        EmbeddingProvider::None => Ok(Arc::new(NullEmbedder::new())),
        EmbeddingProvider::Openai | EmbeddingProvider::Ollama => build_remote_embedder(cfg),
    }
}

#[cfg(feature = "remote-embed")]
fn build_remote_embedder(
    cfg: &lore_scope::config::Config,
) -> AnyhowResult<Arc<dyn EmbeddingService>> {
    let embedder = RemoteEmbedder::new(
        cfg.embedding_url.clone(),
        cfg.embedding_model.clone(),
        cfg.embedding_timeout,
    )
    .context("build remote embedder")?;
    Ok(Arc::new(embedder))
}

#[cfg(not(feature = "remote-embed"))]
fn build_remote_embedder(
    _cfg: &lore_scope::config::Config,
) -> AnyhowResult<Arc<dyn EmbeddingService>> {
    anyhow::bail!("remote embedding support is disabled at compile time")
}

/// Build the storage backend.
async fn build_store(
    cfg: &lore_scope::config::Config,
) -> AnyhowResult<Arc<dyn ExperienceRepository>> {
    let store = SQLiteVecStore::open(&cfg.db_path, cfg.vector_dim)
        .await
        .context("open SQLite store")?;
    Ok(Arc::new(store))
}

/// Build the retrieval engine based on the configured `retrieval_mode`.
///
/// Returns an un-`Arc`-wrapped engine so the caller can chain
/// `with_external_registry` before wrapping in `Arc` for sharing.
fn build_retrieval_engine(
    cfg: &lore_scope::config::Config,
    embedder: Arc<dyn EmbeddingService>,
    store: Arc<dyn ExperienceRepository>,
) -> RetrievalEngine {
    let mode = cfg.retrieval_mode;
    RetrievalEngine::new(embedder, store, mode)
}

/// Tool: search character knowledge graph (`character_search`).
struct CharacterSearchTool {
    store: Arc<SQLiteCharacterStore>,
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
struct CharacterNetworkTool {
    store: Arc<SQLiteCharacterStore>,
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
struct CharacterIngestTool {
    store: Arc<SQLiteCharacterStore>,
}

#[async_trait::async_trait]
impl ToolHandler for CharacterIngestTool {
    async fn call(&self, args: &serde_json::Value) -> Result<ToolCallResult, Error> {
        let corpus_dir = args
            .get("corpus_dir")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("corpus");

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
struct CharacterGraphTool {
    store: Arc<SQLiteCharacterStore>,
}

#[async_trait::async_trait]
impl ToolHandler for CharacterGraphTool {
    async fn call(&self, args: &serde_json::Value) -> Result<ToolCallResult, Error> {
        let tenant_id = args
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("novels");
        let novel = args.get("novel").and_then(serde_json::Value::as_str);
        let max_nodes = args
            .get("max_nodes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(200) as usize;

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

/// Build the MCP server with all `memory_*` and `character_*` tools registered.
async fn build_server(
    cfg: &lore_scope::config::Config,
) -> AnyhowResult<(MCPServer, Arc<PipelineDistiller>, Arc<RetrievalEngine>)> {
    let store = build_store(cfg).await?;
    let embedder = build_embedder(cfg)?;

    // Shared external-knowledge registry (external-knowledge-plan §B/E).
    // Created once here and shared (via Arc) between:
    // - the retrieval engine (hybrid search fuses external signals via RRF),
    // - the knowledge_attach/ingest MCP tools (runtime mutation via RwLock).
    let external_registry = Arc::new(lore_scope::knowledge::ExternalKnowledgeRegistry::new());
    let engine = Arc::new(
        build_retrieval_engine(cfg, embedder.clone(), store.clone())
            .with_external_registry(external_registry.clone()),
    );

    let distill_cfg = DistillationConfig {
        min_importance: cfg.min_importance,
        conflict_threshold: cfg.conflict_threshold,
        max_memories_per_distillation: cfg.max_memories_per_distillation,
        max_solutions_per_tenant: cfg.max_solutions_per_tenant,
        enable_cross_turn: cfg.enable_cross_turn,
    };
    let distiller = Arc::new(PipelineDistiller::new(
        distill_cfg,
        embedder.clone(),
        store.clone(),
    ));

    let mut builder = ServerBuilder::new(Implementation {
        name: "cognitive-memory-mcp".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    });

    // lore_scope
    builder = builder
        .tool(
            ToolDefinition {
                name: "lore_scope".into(),
                description: "Distill memories from a conversation's messages".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "conversation_id": {"type": "string"},
                        "messages": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "role": {"type": "string", "enum": ["user", "assistant", "system"]},
                                    "content": {"type": "string"},
                                    "tool_call_id": {"type": "string"},
                                    "turn_id": {"type": "string"}
                                },
                                "required": ["role", "content"]
                            }
                        },
                        "tenant_id": {"type": "string", "default": "default"},
                        "user_id": {"type": "string"}
                    },
                    "required": ["conversation_id", "messages"]
                }),
            },
            Arc::new(MemoryDistillTool {
                distiller: distiller.clone(),
            }),
        )
        .await;

    // memory_search
    builder = builder
        .tool(
            ToolDefinition {
                name: "memory_search".into(),
                description: "Search memories within a tenant (keyword/vector/hybrid)".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "tenant_id": {"type": "string", "default": "default"},
                        "limit": {"type": "integer", "default": 5},
                        "memory_type": {
                            "type": "string",
                            "enum": ["knowledge", "preference", "skill", "experience", "interaction", "profile"]
                        }
                    },
                    "required": ["query"]
                }),
            },
            Arc::new(MemorySearchTool {
                engine: engine.clone(),
            }),
        )
        .await;

    // memory_store
    builder = builder
        .tool(
            ToolDefinition {
                name: "memory_store".into(),
                description: "Manually store a memory".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "content": {"type": "string"},
                        "tenant_id": {"type": "string", "default": "default"},
                        "memory_type": {
                            "type": "string",
                            "enum": ["knowledge", "preference", "skill", "experience", "interaction", "profile"],
                            "default": "knowledge"
                        },
                        "confidence": {"type": "number", "default": 0.5}
                    },
                    "required": ["content"]
                }),
            },
            Arc::new(MemoryStoreTool {
                store: store.clone(),
            }),
        )
        .await;

    // memory_feedback
    builder = builder
        .tool(
            ToolDefinition {
                name: "memory_feedback".into(),
                description: "Record agent feedback on a memory (for Evolution)".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "memory_id": {"type": "string"},
                        "useful": {"type": "boolean", "default": true}
                    },
                    "required": ["memory_id"]
                }),
            },
            Arc::new(MemoryFeedbackTool {
                store: store.clone(),
            }),
        )
        .await;

    // ── Shared fact store for all cognition / persona / relationship / decay
    //    tools. A single connection is opened once and shared by value (Arc)
    //    so the HTTP server never opens a fresh SQLite handle per tool, which
    //    avoids write-lock contention across concurrent requests.
    let shared_fact_store =
        Arc::new(SqliteFactStore::open(&cfg.db_path).context("open shared fact store")?);

    // memory_compile
    builder = builder
        .tool(
            memory_compile_definition(),
            Arc::new(MemoryCompileTool::new(
                Some(distiller.clone()),
                shared_fact_store.clone(),
            )),
        )
        .await;

    // memory_context_check — proactive context-aware memory (40% threshold
    // auto-distill + user profile). Shares the same fact store + distiller.
    builder = builder
        .tool(
            context_check_definition(),
            Arc::new(ContextCheckTool::new(
                Some(distiller.clone()),
                shared_fact_store.clone(),
            )),
        )
        .await;

    // ── Persona consistency guard (persona_check) ─────────────
    //
    // `persona_check` — the "人设不崩" guard. Given an agent's draft reply and
    // the accumulated `agent_personality` facts for that agent entity, it
    // reports contradictions (conflicts) and unanchored statements (drift).
    // No LLM: embedding semantic match with keyword fallback; read-only.
    builder = builder
        .tool(
            persona_check_definition(),
            Arc::new(PersonaCheckTool::new(shared_fact_store.clone(), embedder.clone()).await),
        )
        .await;

    // ── Persona injection (persona_inject) ───────────────────
    //
    // `persona_inject` — the companion entry point. Builds a structured
    // persona card (identity / persona / style / taboos / relationship) for
    // an agent either from the accumulated `agent_personality` facts or from
    // an imported JSON persona-card file, and returns it as text or JSON to
    // be spliced into the host's system prompt. No LLM — deterministic.
    builder = builder
        .tool(
            persona_inject_definition(),
            Arc::new(PersonaInjectTool::new(shared_fact_store.clone())),
        )
        .await;

    // ── Relationship state + evolution timeline ──────────────
    //
    // `relationship_update` — incrementally updates the intimacy/stage/emotion
    //   trend/recent-topics state from a dialog's emotion signals.
    // `relationship_query` — reads the current relationship snapshot.
    // `persona_timeline` — rebuilds a person's full evolution trajectory
    //   (起点 → 关键转变点 → 现状) under mem0 v3 ADD-only accumulation.
    builder = builder
        .tool(
            relationship_update_definition(),
            Arc::new(RelationshipUpdateTool::new(shared_fact_store.clone())),
        )
        .await
        .tool(
            relationship_query_definition(),
            Arc::new(RelationshipQueryTool::new(shared_fact_store.clone())),
        )
        .await
        .tool(
            persona_timeline_definition(),
            Arc::new(PersonaTimelineTool::new(shared_fact_store.clone())),
        )
        .await;

    // ── Memory decay / forgetting (memory_decay) ─────────────
    //
    // `memory_decay` — applies configurable decay (time / importance / access
    //   frequency / hybrid) to accumulated facts. It only downweights (writes
    //   `weight`/`archived`) and NEVER deletes historical facts, so the
    //   persona evolution timeline stays fully reconstructible.
    builder = builder
        .tool(
            memory_decay_definition(),
            Arc::new(MemoryDecayTool::new(shared_fact_store.clone())),
        )
        .await;

    // ── Background memory decay (plan D2, opt-in) ─────────────
    //
    // The MCP `memory_decay` tool above is the manual on-demand entry point.
    // When `MEMORY_DECAY_INTERVAL_SECS` is set to a positive integer, a
    // tokio task runs a decay pass every interval over all entities. Disabled
    // by default so the server's behaviour is unchanged unless asked for.
    if let Some(interval_secs) = std::env::var("MEMORY_DECAY_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
    {
        let decay_store = shared_fact_store.clone();
        let decay_config = Arc::new(DecayConfig::load());
        let decay_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        tokio::spawn(async move {
            let _ = run_decay_loop(
                decay_store.clone(),
                decay_config,
                std::time::Duration::from_secs(interval_secs),
                decay_stop,
            )
            .await;
        });
        tracing::info!("background memory decay enabled: every {interval_secs}s");
    }

    // ── Character knowledge tools ─────────────────────────────

    let char_store = Arc::new(
        SQLiteCharacterStore::open(&cfg.db_path)
            .await
            .context("open character store")?,
    );

    // character_search
    builder = builder
        .tool(
            ToolDefinition {
                name: "character_search".into(),
                description: "[LEGACY novel-domain tool] Search characters in the classical-novel graph. Prefer the general `search_graph` tool, which searches all entity types across the unified graph.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query – matches name, clothing, personality, description"},
                        "tenant_id": {"type": "string", "default": "novels"},
                        "novel": {"type": "string", "description": "Optional novel filter (e.g. 水浒传, 西游记, 三国演义, 红楼梦)"},
                        "limit": {"type": "integer", "default": 10},
                        "include_events": {"type": "boolean", "default": false, "description": "Include events for each character"},
                        "include_relations": {"type": "boolean", "default": false, "description": "Include relations for each character"}
                    },
                    "required": ["query"]
                }),
            },
            Arc::new(CharacterSearchTool {
                store: char_store.clone(),
            }),
        )
        .await;

    // character_network
    builder = builder
        .tool(
            ToolDefinition {
                name: "character_network".into(),
                description: "[LEGACY novel-domain tool] Traverse the classical-novel character graph via BFS. Prefer the general `relation_graph` tool for the unified graph.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Starting character name"},
                        "tenant_id": {"type": "string", "default": "novels"},
                        "novel": {"type": "string", "description": "Optional novel filter"},
                        "depth": {"type": "integer", "default": 2, "description": "Traversal depth (1-5, default 2)"}
                    },
                    "required": ["name"]
                }),
            },
            Arc::new(CharacterNetworkTool {
                store: char_store.clone(),
            }),
        )
        .await;

    // character_ingest — run the corpus distillation pipeline
    builder = builder
        .tool(
            ToolDefinition {
                name: "character_ingest".into(),
                description: "[LEGACY novel-domain tool] Distill the classical-novel character graph from corpus text files. Prefer `generalize_compile`, which ingests arbitrary data (dialog or prose) into the unified graph.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "corpus_dir": {"type": "string", "default": "corpus", "description": "Path to directory containing novel .txt files (水浒传.txt, 三国演义.txt, 红楼梦.txt, 西游记.txt)"}
                    }
                }),
            },
            Arc::new(CharacterIngestTool {
                store: char_store.clone(),
            }),
        )
        .await;

    // character_graph — export 3D relationship graph for visualization
    builder = builder
        .tool(
            ToolDefinition {
                name: "character_graph".into(),
                description: "[LEGACY novel-domain tool] Export the classical-novel character relationship graph as structured JSON for visualization. For the unified graph use `relation_graph` or `search_graph`.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tenant_id": {"type": "string", "default": "novels"},
                        "novel": {"type": "string", "description": "Optional novel filter (e.g. 水浒传)"},
                        "max_nodes": {"type": "integer", "default": 200, "description": "Maximum nodes to return"}
                    }
                }),
            },
            Arc::new(CharacterGraphTool {
                store: char_store.clone(),
            }),
        )
        .await;

    // ── General knowledge model tools (dev_guide §5) ───────────
    //
    // The general knowledge store is opened against the same SQLite file as
    // the character store: V1 stays as a legacy read view (dev_guide §6
    // "不双写"), while the four new tools query the general tables produced
    // by `lore-scope migrate`. Opening the store here is idempotent
    // (CREATE TABLE IF NOT EXISTS), so `serve` works whether or not a
    // migration has been run.
    let kstore = Arc::new(
        SQLiteKnowledgeStore::open(&cfg.db_path)
            .await
            .context("open knowledge store")?,
    );
    // memory_stats — memory health for a tenant: distilled memories by type
    // plus knowledge-graph health (documents / entities / relations /
    // evidence), so an agent can gauge persona-memory completeness at a
    // glance. Registered here (after kstore) because it reports both stores.
    builder = builder
        .tool(
            ToolDefinition {
                name: "memory_stats".into(),
                description: "Aggregate memory health for a tenant: distilled memories by type plus knowledge-graph counts (documents/entities/relations/evidence)".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tenant_id": {"type": "string", "default": "default"}
                    }
                }),
            },
            Arc::new(MemoryStatsTool {
                store: store.clone(),
                kgraph: kstore.clone(),
            }),
        )
        .await;
    let fact_store_knowledge = Arc::new(
        SqliteFactStore::open(&cfg.db_path).context("open fact store for knowledge tools")?,
    );
    // Shared cross-source entity linker (external-knowledge-plan §D3). Starts
    // empty; `knowledge_attach` (Phase E) rebuilds it after attaching a
    // source. `inspect_entity` reads it to resolve external surface names to
    // unified graph nodes.
    let entity_linker: Arc<std::sync::RwLock<lore_scope::knowledge::EntityLinker>> = Arc::new(
        std::sync::RwLock::new(lore_scope::knowledge::EntityLinker::new()),
    );
    builder = register_knowledge_tools(
        builder,
        kstore.clone(),
        fact_store_knowledge.clone(),
        Some(entity_linker.clone()),
    )
    .await;

    // ── External knowledge tools (external-knowledge-plan §E) ─────────
    //
    // Three tools that let the agent attach external knowledge sources
    // (PDF/JSON/TXT/MD documents, JSON-backed DBs), materialize them into
    // the graph, and compile AI conversations into three-state facts. They
    // share the same registry + linker as the retrieval engine and
    // inspect_entity so a runtime attach is immediately visible to search.
    builder = register_external_knowledge_tools(
        builder,
        external_registry,
        entity_linker,
        kstore.clone(),
        fact_store_knowledge,
    )
    .await;

    // ── Generalize tool (generalization plan) ─────────────────────────
    //
    // `generalize_compile` — the production entry point for the unified
    // DocumentSource + DomainProfile + compile_source pipeline. It compiles
    // ANY caller-provided data (pasted conversation or raw prose) into the
    // knowledge graph, wiring the generalization modules into a live tool so
    // external data can be ingested and later retrieved to sustain the AI
    // persona.
    builder = register_generalize_tool(builder, kstore.clone()).await;

    // ── Memory transfer tools (memory migration) ──────────────────────
    //
    // `memory_export` / `memory_import` — serialize the whole knowledge graph
    // into a portable JSON snapshot and replay it back, so a persona's memory
    // can be backed up, moved between machines, or shared and restored intact
    // (the "memory is never lost" guarantee made concrete).
    builder = register_memory_transfer_tools(builder, kstore.clone()).await;

    // ── Structured graph search ──────────────────────────────────────
    //
    // `search_graph` — one structured query (by name/type/attribute) returns
    // matched entities with their document, confidence, and relation count,
    // replacing many single-entity lookups for the agent.
    builder = register_graph_search_tool(builder, kstore.clone()).await;

    // ── Relationship-path tracing ──────────────────────────────────
    //
    // `trace_path` — shortest hop-by-hop path between two entities (BFS over
    // graph edges), answering "how are these two connected?" in one call.
    builder = register_trace_path_tool(builder, kstore.clone()).await;

    // person_key_events — distill a person's full trajectory into key events
    // (importance score + turning flag + evidence anchors). Supplies evidence
    // only; the AI consuming this tool performs the analysis.
    builder = builder
        .tool(
            key_events_definition(),
            Arc::new(KeyEventsTool::new(kstore.clone())),
        )
        .await;

    // story_bridge — bridge a novel character's story events (knowledge graph)
    // into fact-store persona facts so `persona_timeline` works on novel corpus
    // (acceptance item 8, novel side). Reads the knowledge graph, writes facts.
    builder = builder
        .tool(
            story_bridge_definition(),
            Arc::new(StoryBridgeTool::new(
                kstore.clone(),
                shared_fact_store.clone(),
            )),
        )
        .await;

    Ok((builder.build(), distiller, engine))
}

#[tokio::main]
async fn main() -> AnyhowResult<()> {
    // Initialize logging.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr) // keep stdout clean for JSON-RPC
        .init();

    let cli = CliArgs::parse();
    // Read transport options up front (before `match cli.command` partially
    // moves `command`), so the serve branch can dispatch on them without
    // touching the moved field.
    let transport_kind = cli.transport.clone();
    let http_addr = cli.http_addr.clone();
    let http_token = cli.http_token.clone();
    match cli.command {
        Some(Command::Serve) | None => {
            let cfg = cli.into_config().context("load configuration")?;
            let (server, _distiller, _engine) = build_server(&cfg).await?;
            match transport_kind.as_str() {
                "http" => {
                    let addr: std::net::SocketAddr = http_addr
                        .parse()
                        .context("invalid --http-addr; expected host:port")?;
                    serve_http_addr(server, addr, http_token).await?;
                }
                "stdio" => {
                    let mut transport = StdioTransport::new();
                    server.serve(&mut transport).await?;
                }
                other => {
                    anyhow::bail!("unsupported --transport `{other}` (expected `stdio` or `http`)");
                }
            }
        }
        Some(Command::Ingest { corpus_dir }) => {
            // `cli.command` is moved by this binding, so read `db_path` directly
            // from the remaining (un-moved) fields instead of `into_config()`.
            let store = Arc::new(
                SQLiteCharacterStore::open(&cli.db_path)
                    .await
                    .context("open character store")?,
            );
            let pipeline = IngestionPipeline::new(store, &corpus_dir);
            let stats = pipeline.run().await.context("distill character corpus")?;
            println!(
                "Ingestion complete: {} characters, {} events, {} relations",
                stats.characters, stats.events, stats.relations
            );
        }
        Some(Command::Migrate { corpus_dir }) => {
            // Open both stores against the same SQLite file: V1 is read-only
            // here, the general model is written. A prior `ingest` run is
            // expected to have populated the V1 `character_*` tables.
            let v1 = SQLiteCharacterStore::open(&cli.db_path)
                .await
                .context("open character store for migration")?;
            let knowledge = SQLiteKnowledgeStore::open(&cli.db_path)
                .await
                .context("open knowledge store for migration")?;
            let migrator = Migrator::new(&v1, &knowledge, std::path::Path::new(&corpus_dir));
            let stats = migrator
                .migrate()
                .await
                .context("run V1 → general migration")?;
            println!(
                "Migration complete: {} documents, {} chapters, {} objects, {} edges, {} evidence, {} mentions",
                stats.documents,
                stats.chapters,
                stats.objects,
                stats.edges,
                stats.evidence,
                stats.mentions
            );
        }
    }
    Ok(())
}
