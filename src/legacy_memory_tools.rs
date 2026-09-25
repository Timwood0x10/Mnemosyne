//! Legacy `memory_*` (lore_scope) tool handlers kept on the binary side.

use std::sync::Arc;

use serde_json::Value;

use mnemosyne::distiller::{Distiller, PipelineDistiller};
use mnemosyne::error::Error;
use mnemosyne::knowledge::SQLiteKnowledgeStore;
use mnemosyne::knowledge::store::KnowledgeStore;
use mnemosyne::mcp::types::{ToolCallResult, ToolHandler};
use mnemosyne::retrieval::RetrievalEngine;
use mnemosyne::store::ExperienceRepository;
use mnemosyne::types::{Experience, MemoryType, Message};

/// Tool: distill memories from a conversation (`lore_scope`).
pub(crate) struct MemoryDistillTool {
    pub(crate) distiller: Arc<PipelineDistiller>,
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
pub(crate) struct MemorySearchTool {
    pub(crate) engine: Arc<RetrievalEngine>,
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
        // Reject an invalid memory_type instead of silently swallowing it
        // (the old `.ok()` made the filter a no-op and returned unfiltered
        // results — callers believed the filter had applied).
        let memory_type_filter = match args.get("memory_type").and_then(Value::as_str) {
            Some(s) => Some(
                s.parse::<MemoryType>()
                    .map_err(|e| Error::InvalidInput(format!("invalid memory_type `{s}`: {e}")))?,
            ),
            None => None,
        };

        let results = self
            .engine
            .search(query, tenant_id, limit, memory_type_filter)
            .await?;
        let payload = serde_json::json!({ "results": results });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Tool: manually store a memory (`memory_store`).
pub(crate) struct MemoryStoreTool {
    pub(crate) store: Arc<dyn ExperienceRepository>,
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
        // Reject an invalid memory_type instead of silently defaulting to
        // Knowledge — the same contract `memory_search` already enforces
        // (a typo'd filter must not be reported as a working filter).
        let memory_type = match args.get("memory_type").and_then(Value::as_str) {
            Some(s) => s
                .parse::<MemoryType>()
                .map_err(|e| Error::InvalidInput(format!("invalid memory_type `{s}`: {e}")))?,
            None => MemoryType::Knowledge,
        };
        let confidence = args
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);

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
pub(crate) struct MemoryFeedbackTool {
    pub(crate) store: Arc<dyn ExperienceRepository>,
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
        // Optional tenant binding: a leaked UUID must not let any client
        // rewrite another tenant's confidence/votes.
        let tenant_filter = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty());

        let Some(mut exp) = self.store.get(memory_id).await? else {
            return Ok(ToolCallResult::text(format!(
                "memory `{memory_id}` not found; feedback not applied"
            )));
        };
        if let Some(expected) = tenant_filter
            && exp.tenant_id != expected
        {
            return Ok(ToolCallResult::text(format!(
                "memory `{memory_id}` not found; feedback not applied"
            )));
        }

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
pub(crate) struct MemoryStatsTool {
    pub(crate) store: Arc<dyn ExperienceRepository>,
    pub(crate) kgraph: Arc<SQLiteKnowledgeStore>,
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
