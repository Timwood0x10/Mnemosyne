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
use lore_scope::distiller::{DistillationConfig, Distiller, PipelineDistiller};
#[cfg(feature = "remote-embed")]
use lore_scope::embed::RemoteEmbedder;
use lore_scope::embed::{EmbeddingService, NullEmbedder};
use lore_scope::error::Error;
use lore_scope::fact_store::SqliteFactStore;
use lore_scope::ingest::IngestionPipeline;
use lore_scope::knowledge::{Migrator, SQLiteKnowledgeStore};
use lore_scope::mcp::context_aware::{ContextCheckTool, context_check_definition};
use lore_scope::mcp::memory_compile::{MemoryCompileTool, memory_compile_definition};
use lore_scope::mcp::portrait_tool::{PortraitTool, portrait_extract_definition};
use lore_scope::mcp::register_external_knowledge_tools;
use lore_scope::mcp::register_knowledge_tools;
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
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;
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
struct MemoryFeedbackTool;

#[async_trait::async_trait]
impl ToolHandler for MemoryFeedbackTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let memory_id = args
            .get("memory_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `memory_id`".into()))?;
        let useful = args.get("useful").and_then(Value::as_bool).unwrap_or(true);

        // For now, feedback is logged via tracing. Future Evolution work
        // will persist feedback and adjust importance decay.
        tracing::info!(
            memory_id = %memory_id,
            useful = %useful,
            "memory feedback recorded"
        );
        Ok(ToolCallResult::text(format!(
            "feedback recorded for memory `{memory_id}` (useful={useful})"
        )))
    }
}

/// Tool: aggregate stats for a tenant (`memory_stats`).
struct MemoryStatsTool {
    store: Arc<dyn ExperienceRepository>,
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
        let payload = serde_json::json!({
            "total_memories": total,
            "by_type": by_type,
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
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(50) as usize;
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
            Arc::new(MemoryFeedbackTool),
        )
        .await;

    // memory_stats
    builder = builder
        .tool(
            ToolDefinition {
                name: "memory_stats".into(),
                description: "Aggregate memory stats for a tenant".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tenant_id": {"type": "string", "default": "default"}
                    }
                }),
            },
            Arc::new(MemoryStatsTool {
                store: store.clone(),
            }),
        )
        .await;

    // memory_compile
    let compile_fact_store =
        Arc::new(SqliteFactStore::open(&cfg.db_path).context("open fact store for compile tool")?);
    builder = builder
        .tool(
            memory_compile_definition(),
            Arc::new(MemoryCompileTool::new(
                Some(distiller.clone()),
                compile_fact_store.clone(),
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
                compile_fact_store,
            )),
        )
        .await;

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
                description: "Search characters by name/attribute/novel in the knowledge graph".into(),
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
                description: "Traverse the character knowledge graph: character → events → related characters → their events (BFS up to depth)".into(),
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
                description: "Distill character knowledge graph from classical novel corpus text files. Extracts characters, events, descriptions, and relationships. Heavy operation (30-60s).".into(),
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
                description: "Export the 3D character relationship graph as structured JSON: nodes (characters with appearance/personality/action dimensions) + edges (relations with weights) for visualization".into(),
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
        kstore,
        fact_store_knowledge,
    )
    .await;

    // portrait_extract — deterministic resume/person-document portrait via
    // rules (no LLM). Stateless; no store dependency.
    builder = builder
        .tool(portrait_extract_definition(), Arc::new(PortraitTool::new()))
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
    match cli.command {
        Some(Command::Serve) | None => {
            let cfg = cli.into_config().context("load configuration")?;
            let (server, _distiller, _engine) = build_server(&cfg).await?;
            let mut transport = StdioTransport::new();
            server.serve(&mut transport).await?;
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
