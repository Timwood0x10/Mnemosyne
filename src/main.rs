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
use tracing_subscriber::EnvFilter;

use mnemosyne::character::SQLiteCharacterStore;
use mnemosyne::config::{CliArgs, Command, EmbeddingProvider};
use mnemosyne::decay::{DecayConfig, run_decay_loop};
use mnemosyne::distiller::{DistillationConfig, PipelineDistiller};
#[cfg(feature = "remote-embed")]
use mnemosyne::embed::RemoteEmbedder;
use mnemosyne::embed::{EmbeddingService, NullEmbedder};
use mnemosyne::fact_store::SqliteFactStore;
use mnemosyne::ingest::IngestionPipeline;
use mnemosyne::knowledge::{Migrator, SQLiteKnowledgeStore};
use mnemosyne::mcp::context_aware::{ContextCheckTool, context_check_definition};
use mnemosyne::mcp::decay_tool::{MemoryDecayTool, memory_decay_definition};
use mnemosyne::mcp::decision_tool::{
    DecisionSearchTool, DecisionTraceTool, decision_search_definition, decision_trace_definition,
};
use mnemosyne::mcp::key_events_tool::{KeyEventsTool, key_events_definition};
use mnemosyne::mcp::memory_compile::{MemoryCompileTool, memory_compile_definition};
use mnemosyne::mcp::persona_check_tool::{PersonaCheckTool, persona_check_definition};
use mnemosyne::mcp::persona_inject_tool::{PersonaInjectTool, persona_inject_definition};
use mnemosyne::mcp::provenance_tool::{FactProvenanceTool, fact_provenance_definition};
use mnemosyne::mcp::register_external_knowledge_tools;
use mnemosyne::mcp::register_generalize_tool;
use mnemosyne::mcp::register_graph_search_tool;
use mnemosyne::mcp::register_knowledge_tools;
use mnemosyne::mcp::register_memory_transfer_tools;
use mnemosyne::mcp::register_trace_path_tool;
use mnemosyne::mcp::relationship_tool::{
    PersonaTimelineTool, RelationshipQueryTool, RelationshipUpdateTool,
    persona_timeline_definition, relationship_query_definition, relationship_update_definition,
};
use mnemosyne::mcp::serve_http_addr;
use mnemosyne::mcp::state_timeline_tool::{StateTimelineTool, state_timeline_definition};
use mnemosyne::mcp::story_bridge_tool::{StoryBridgeTool, story_bridge_definition};
use mnemosyne::mcp::types::{Implementation, ToolDefinition};
use mnemosyne::mcp::{MCPServer, ServerBuilder, StdioTransport};
use mnemosyne::retrieval::RetrievalEngine;
use mnemosyne::store::{ExperienceRepository, SQLiteVecStore};

mod character_tools;
mod legacy_memory_tools;

use character_tools::{
    CharacterGraphTool, CharacterIngestTool, CharacterNetworkTool, CharacterSearchTool,
};
use legacy_memory_tools::{
    MemoryDistillTool, MemoryFeedbackTool, MemorySearchTool, MemoryStatsTool, MemoryStoreTool,
};

/// Build the embedder based on the configured `embedding_provider`.
fn build_embedder(cfg: &mnemosyne::config::Config) -> AnyhowResult<Arc<dyn EmbeddingService>> {
    match cfg.embedding_provider {
        EmbeddingProvider::None => Ok(Arc::new(NullEmbedder::new())),
        EmbeddingProvider::Openai | EmbeddingProvider::Ollama => build_remote_embedder(cfg),
    }
}

#[cfg(feature = "remote-embed")]
fn build_remote_embedder(
    cfg: &mnemosyne::config::Config,
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
    _cfg: &mnemosyne::config::Config,
) -> AnyhowResult<Arc<dyn EmbeddingService>> {
    anyhow::bail!("remote embedding support is disabled at compile time")
}

/// Build the storage backend.
async fn build_store(
    cfg: &mnemosyne::config::Config,
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
    cfg: &mnemosyne::config::Config,
    embedder: Arc<dyn EmbeddingService>,
    store: Arc<dyn ExperienceRepository>,
) -> RetrievalEngine {
    let mode = cfg.retrieval_mode;
    RetrievalEngine::new(embedder, store, mode)
}

/// Build the MCP server with all `memory_*` and `character_*` tools registered.
async fn build_server(
    cfg: &mnemosyne::config::Config,
) -> AnyhowResult<(MCPServer, Arc<PipelineDistiller>, Arc<RetrievalEngine>)> {
    let store = build_store(cfg).await?;
    let embedder = build_embedder(cfg)?;

    // Shared external-knowledge registry (external-knowledge-plan §B/E).
    // Created once here and shared (via Arc) between:
    // - the retrieval engine (hybrid search fuses external signals via RRF),
    // - the knowledge_attach/ingest MCP tools (runtime mutation via RwLock).
    let external_registry = Arc::new(mnemosyne::knowledge::ExternalKnowledgeRegistry::new());
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

    // ── Fact provenance (fact_provenance) ────────────────────
    //
    // `fact_provenance` — audits why a cognitive fact is believed: confidence,
    // epistemic status (active/superseded/contradicted), the original-text
    // evidence anchor, and the derived_from derivation chain. Read-only.
    builder = builder
        .tool(
            fact_provenance_definition(),
            Arc::new(FactProvenanceTool::new(shared_fact_store.clone())),
        )
        .await;

    // ── Cognitive state history (state_timeline) ─────────────
    //
    // `state_timeline` — returns how an entity's cognitive state emerged:
    // per-dimension state intervals (validity windows + evidence) and
    // deterministic transitions between them (gradual/stance-flip/behavioral
    // confirmation). ADD-only, read-only; a change without a definite signal
    // is reported as intervals only.
    builder = builder
        .tool(
            state_timeline_definition(),
            Arc::new(StateTimelineTool::new(shared_fact_store.clone())),
        )
        .await;

    // ── Decisions (decision_trace / decision_search, v0.3.1) ─
    //
    // `decision_trace` — traces a decision back to the facts that supported
    //   it (supporting evidence, not causality).
    // `decision_search` — lightweight keyword search over a subject's
    //   decisions. Both read-only; decisions follow their own lifecycle and
    //   do NOT touch memory_decay semantics.
    builder = builder
        .tool(
            decision_trace_definition(),
            Arc::new(DecisionTraceTool::new(shared_fact_store.clone())),
        )
        .await
        .tool(
            decision_search_definition(),
            Arc::new(DecisionSearchTool::new(shared_fact_store.clone())),
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
    // by `mnemosyne migrate`. Opening the store here is idempotent
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
    let entity_linker: Arc<std::sync::RwLock<mnemosyne::knowledge::EntityLinker>> = Arc::new(
        std::sync::RwLock::new(mnemosyne::knowledge::EntityLinker::new()),
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
                    // HTTP serving exposes the MCP server over the network;
                    // require an explicit token so an unauthenticated listener
                    // is never started by accident.
                    let token = http_token
                        .ok_or_else(|| anyhow::anyhow!(
                            "HTTP transport requires --http-token (refusing to serve without authentication)"
                        ))?;
                    serve_http_addr(server, addr, Some(token)).await?;
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
