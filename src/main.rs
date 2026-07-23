//! Main entry point for the Cognitive Memory MCP Server.
//!
//! Wires together the configuration, store, embedder, distiller, retrieval
//! engine, and MCP server, then registers the five `memory_*` MCP tools
//! (`memory_distill`, `memory_search`, `memory_store`, `memory_feedback`,
//! `memory_stats`) and runs the protocol loop over stdio.

use std::sync::Arc;

use anyhow::{Context, Result as AnyhowResult};
use clap::Parser;
use serde_json::Value;
use tracing_subscriber::EnvFilter;

use memory_distill::compiler::ConversationCompiler;
use memory_distill::config::{CliArgs, Command, EmbeddingProvider};
use memory_distill::distiller::{DistillationConfig, Distiller, PipelineDistiller};
use memory_distill::embed::{EmbeddingService, NullEmbedder, RemoteEmbedder};
use memory_distill::error::Error;
use memory_distill::mcp::types::{Implementation, ToolCallResult, ToolDefinition, ToolHandler};
use memory_distill::mcp::{MCPServer, ServerBuilder, StdioTransport};
use memory_distill::prompt::PromptBuilder;
use memory_distill::retrieval::RetrievalEngine;
use memory_distill::store::{ExperienceRepository, SQLiteVecStore};
use memory_distill::types::{Experience, MemoryType, Message};

/// Tool: distill memories from a conversation (`memory_distill`).
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
#[allow(dead_code)]
struct MemoryFeedbackTool {
    store: Arc<dyn ExperienceRepository>,
}

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

/// Parse the `messages` array from a `memory_distill` tool call.
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
fn build_embedder(cfg: &memory_distill::config::Config) -> AnyhowResult<Arc<dyn EmbeddingService>> {
    match cfg.embedding_provider {
        EmbeddingProvider::None => Ok(Arc::new(NullEmbedder::new())),
        EmbeddingProvider::Openai | EmbeddingProvider::Ollama => {
            let embedder = RemoteEmbedder::new(
                cfg.embedding_url.clone(),
                cfg.embedding_model.clone(),
                cfg.embedding_timeout,
            )
            .context("build remote embedder")?;
            Ok(Arc::new(embedder))
        }
    }
}

/// Build the storage backend.
async fn build_store(
    cfg: &memory_distill::config::Config,
) -> AnyhowResult<Arc<dyn ExperienceRepository>> {
    let store = SQLiteVecStore::open(&cfg.db_path, cfg.vector_dim)
        .await
        .context("open SQLite store")?;
    Ok(Arc::new(store))
}

/// Build the retrieval engine based on the configured `retrieval_mode`.
fn build_retrieval_engine(
    cfg: &memory_distill::config::Config,
    embedder: Arc<dyn EmbeddingService>,
    store: Arc<dyn ExperienceRepository>,
) -> Arc<RetrievalEngine> {
    let mode = cfg.retrieval_mode;
    Arc::new(RetrievalEngine::new(embedder, store, mode))
}

/// Tool: compile conversation into structured state (`memory_compile`).
/// Optionally distills and builds a reconstruction prompt.
struct MemoryCompileTool {
    distiller: Option<Arc<PipelineDistiller>>,
}

#[async_trait::async_trait]
impl ToolHandler for MemoryCompileTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let messages_raw = args
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidInput("missing `messages` array".into()))?;
        let messages = parse_messages(messages_raw)?;

        let compiler = ConversationCompiler::new();
        let compiled = compiler.compile(&messages);

        // Build reconstruction prompt
        let builder = PromptBuilder;
        let recent_count = std::cmp::min(messages.len(), 6);
        let prompt = builder.build(&messages[messages.len() - recent_count..], &compiled);

        // Optionally run distillation
        let distill = args.get("distill").and_then(Value::as_bool).unwrap_or(true);
        let memories = if distill {
            match &self.distiller {
                Some(d) => {
                    let conv_id = args
                        .get("conversation_id")
                        .and_then(Value::as_str)
                        .unwrap_or("compile");
                    let tenant_id = args
                        .get("tenant_id")
                        .and_then(Value::as_str)
                        .unwrap_or("default");
                    let user_id = args.get("user_id").and_then(Value::as_str).unwrap_or("");

                    // Run distillation pipeline — persists knowledge memories
                    let memories = d.distill(conv_id, &messages, tenant_id, user_id).await?;

                    // Also persist decisions as Knowledge-type memories
                    for dec in &compiled.decisions {
                        let content =
                            format!("Decision: {} — Rationale: {}", dec.decision, dec.rationale);
                        let mut exp = Experience::new(
                            tenant_id,
                            MemoryType::Knowledge,
                            content,
                            dec.importance,
                        );
                        exp.source = "compile".to_string();
                        d.store().create(&exp).await?;
                    }

                    Some(memories)
                }
                None => None,
            }
        } else {
            None
        };

        let payload = serde_json::json!({
            "knowledge": compiled.knowledge,
            "decisions": compiled.decisions,
            "session": compiled.session,
            "prompt": prompt,
            "distilled_memories": memories,
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Build the MCP server with all 6 `memory_*` tools registered.
async fn build_server(
    cfg: &memory_distill::config::Config,
) -> AnyhowResult<(MCPServer, Arc<PipelineDistiller>, Arc<RetrievalEngine>)> {
    let store = build_store(cfg).await?;
    let embedder = build_embedder(cfg)?;
    let engine = build_retrieval_engine(cfg, embedder.clone(), store.clone());

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

    // memory_distill
    builder = builder
        .tool(
            ToolDefinition {
                name: "memory_distill".into(),
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
    builder = builder
        .tool(
            ToolDefinition {
                name: "memory_compile".into(),
                description: "Compile conversation into structured knowledge + decisions + session state. Optionally distill memories.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "messages": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "role": {"type": "string", "enum": ["user", "assistant", "system"]},
                                    "content": {"type": "string"}
                                },
                                "required": ["role", "content"]
                            }
                        },
                        "distill": {"type": "boolean", "default": false, "description": "Also run distillation pipeline"},
                        "conversation_id": {"type": "string", "description": "Required when distill=true"},
                        "tenant_id": {"type": "string", "default": "default"},
                        "user_id": {"type": "string"}
                    },
                    "required": ["messages"]
                }),
            },
            Arc::new(MemoryCompileTool {
                distiller: Some(distiller.clone()),
            }),
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
    match cli.command {
        Some(Command::Serve) | None => {
            let cfg = cli.into_config().context("load configuration")?;
            let (server, _distiller, _engine) = build_server(&cfg).await?;
            let mut transport = StdioTransport::new();
            server.serve(&mut transport).await?;
        }
    }
    Ok(())
}
