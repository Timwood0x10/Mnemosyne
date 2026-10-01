//! `memory_context_check` MCP tool — proactive context-aware memory.
//!
//! ## Design (user request: 主动的上下文感知)
//!
//! The agent host reports its context-window usage percentage. When usage
//! exceeds the configured threshold (default 40%), the tool automatically
//! switches to distillation mode: it compiles the conversation into Facts,
//! persists them, runs the distillation pipeline, and (re)builds a structured
//! **user profile** from the accumulated cognitive facts (preferences, goals,
//! emotions, identity, occupation).
//!
//! Below the threshold the tool is a no-op diagnostic: it reports the current
//! context usage and memory counts without writing anything, so the host can
//! decide when to call it.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::cognition::{FactStore, FactType};
use crate::cognition_compiler::CognitionCompiler;
use crate::config::CONTEXT_INJECT_THRESHOLD;
use crate::distiller::{Distiller, PipelineDistiller};
use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{
    DEFAULT_IDENTITY, ToolCallResult, ToolDefinition, ToolHandler, identity_arg,
};
use crate::types::Message;

/// Backward-compatible alias for the default context-usage threshold.
///
/// The single source of truth is [`CONTEXT_INJECT_THRESHOLD`] in `config`;
/// this re-export keeps existing callers of `DEFAULT_CONTEXT_THRESHOLD`
/// compiling while the tool defaults flow from config.
pub const DEFAULT_CONTEXT_THRESHOLD: f64 = CONTEXT_INJECT_THRESHOLD;

/// Handler for the proactive `memory_context_check` tool.
pub struct ContextCheckTool {
    distiller: Option<Arc<PipelineDistiller>>,
    fact_store: Arc<SqliteFactStore>,
    compiler: Arc<CognitionCompiler>,
}

impl ContextCheckTool {
    /// Create the tool with the shared production stores.
    pub fn new(
        distiller: Option<Arc<PipelineDistiller>>,
        fact_store: Arc<SqliteFactStore>,
    ) -> Self {
        Self {
            distiller,
            fact_store,
            compiler: Arc::new(CognitionCompiler::new()),
        }
    }

    /// Parse the `context_usage_percent` argument; default 0 when absent.
    fn parse_context_usage(args: &Value) -> f64 {
        args.get("context_usage_percent")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 100.0)
    }

    /// Parse the optional `threshold` override; default [`DEFAULT_CONTEXT_THRESHOLD`].
    fn parse_threshold(args: &Value) -> f64 {
        args.get("threshold")
            .and_then(Value::as_f64)
            .unwrap_or(DEFAULT_CONTEXT_THRESHOLD)
            .clamp(1.0, 100.0)
    }
}

#[async_trait::async_trait]
impl ToolHandler for ContextCheckTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let messages_raw = args
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidInput("missing `messages` array".into()))?;
        let messages = parse_messages(messages_raw)?;

        let tenant_id = identity_arg(args, "tenant_id");
        let user_id = identity_arg(args, "user_id");

        let context_usage = Self::parse_context_usage(args);
        let threshold = Self::parse_threshold(args);
        let triggered = context_usage >= threshold;

        if !triggered {
            // The below-threshold diagnostic is a synchronous `rusqlite` read;
            // hand it to the blocking pool so it cannot stall the tokio worker
            // (audit 09-26/H7).
            let fact_store = Arc::clone(&self.fact_store);
            let tenant_id = tenant_id.to_string();
            let user_id = user_id.to_string();
            return crate::mcp::blocking::run(move || {
                run_context_check_diagnostic(
                    &fact_store,
                    &tenant_id,
                    &user_id,
                    context_usage,
                    threshold,
                )
            })
            .await;
        }

        // ── Triggered: compile → persist → distill → profile ────────────
        let fact_store = Arc::clone(&self.fact_store);
        let compiler = Arc::clone(&self.compiler);
        let tenant_id = tenant_id.to_string();
        let user_id = user_id.to_string();
        let compile_messages = messages.clone();
        // Compile + persist is synchronous (compiler + `rusqlite`); run it on
        // the blocking pool so it cannot stall the tokio worker (audit 09-26/H7).
        let summary = {
            let fact_store = Arc::clone(&fact_store);
            let compiler = Arc::clone(&compiler);
            let tenant_id = tenant_id.clone();
            let user_id = user_id.clone();
            crate::mcp::blocking::run(move || {
                run_context_check_compile(
                    &fact_store,
                    &compiler,
                    &tenant_id,
                    &user_id,
                    &compile_messages,
                )
            })
            .await?
        };

        // Distill the conversation into long-term memories (knowledge/preferences…).
        // The network/embedding call stays on the async worker.
        let mut distilled = 0usize;
        if let Some(distiller) = &self.distiller {
            let conversation_id = args
                .get("conversation_id")
                .and_then(Value::as_str)
                .unwrap_or("context-check");
            let memories = distiller
                .distill(conversation_id, &messages, &tenant_id, &user_id)
                .await?;
            distilled = memories.len();
        }

        // Rebuild the user profile from ALL accumulated facts of this user.
        // A synchronous `rusqlite` read, so hand it to the blocking pool.
        let user_entity_id = summary.user_entity_id;
        let profile = {
            let fact_store = Arc::clone(&fact_store);
            crate::mcp::blocking::run(move || build_user_profile(&fact_store, user_entity_id))
                .await?
        };

        let mut payload = serde_json::Map::new();
        payload.insert("context_usage_percent".into(), json!(context_usage));
        payload.insert("threshold_percent".into(), json!(threshold));
        payload.insert("distill_mode".into(), json!(true));
        payload.insert(
            "reason".into(),
            json!("context usage exceeded threshold — distillation activated"),
        );
        payload.insert(
            "compiled".into(),
            json!({
                "observations": summary.observations,
                "facts_compiled": summary.facts_compiled,
                "facts_stored": summary.facts_stored,
                "distilled_memories": distilled,
            }),
        );
        payload.insert("profile".into(), profile);
        Ok(ToolCallResult::text(
            serde_json::Value::Object(payload).to_string(),
        ))
    }
}

/// Diagnostic summary of the synchronous compile+persist step, carried back to
/// the async shell for the response payload.
struct ContextCompileSummary {
    user_entity_id: i64,
    observations: usize,
    facts_compiled: usize,
    facts_stored: usize,
}

/// Synchronous below-threshold body of `memory_context_check`, executed on the
/// blocking pool.
fn run_context_check_diagnostic(
    fact_store: &SqliteFactStore,
    tenant_id: &str,
    user_id: &str,
    context_usage: f64,
    threshold: f64,
) -> Result<ToolCallResult, Error> {
    // No-op diagnostic: report usage and current memory state.
    //
    // Read-only resolve: a below-threshold diagnostic must NOT
    // materialize an entity for a user who never chatted (audit:
    // read-only tools writing via resolve_user). find_entity never
    // writes; unknown users simply report zero facts.
    let name = if user_id == DEFAULT_IDENTITY {
        "User".to_string()
    } else {
        format!("User:{user_id}")
    };
    let user_entity_id = fact_store
        .find_entity(tenant_id, Some(user_id), &name)?
        .map(|(id, _, _)| id);
    let facts = match user_entity_id {
        Some(id) => fact_store.get_facts(id)?,
        None => Vec::new(),
    };
    let mut payload = serde_json::Map::new();
    payload.insert("context_usage_percent".into(), json!(context_usage));
    payload.insert("threshold_percent".into(), json!(threshold));
    payload.insert("distill_mode".into(), json!(false));
    payload.insert(
        "reason".into(),
        json!(format!(
            "context usage {context_usage:.0}% below threshold {threshold:.0}% — no distillation"
        )),
    );
    payload.insert(
        "current".into(),
        json!({
            "user_entity_id": user_entity_id,
            "facts": facts.len(),
        }),
    );
    Ok(ToolCallResult::text(
        serde_json::Value::Object(payload).to_string(),
    ))
}

/// Synchronous compile+persist body of the triggered `memory_context_check`,
/// executed on the blocking pool. The distillation `await` stays in the async
/// shell.
fn run_context_check_compile(
    fact_store: &SqliteFactStore,
    compiler: &CognitionCompiler,
    tenant_id: &str,
    user_id: &str,
    messages: &[Message],
) -> Result<ContextCompileSummary, Error> {
    let user_entity_id = fact_store.resolve_user(tenant_id, user_id)?;
    let logical_time = chrono::Utc::now().timestamp();
    let compiled = compiler.compile_conversation(tenant_id, messages, user_entity_id, logical_time);
    let stored_facts = fact_store.insert_batch(&compiled.facts)?;
    Ok(ContextCompileSummary {
        user_entity_id,
        observations: compiled.observations.len(),
        facts_compiled: compiled.facts.len(),
        facts_stored: stored_facts,
    })
}

/// Return the stable MCP schema for `memory_context_check`.
pub fn context_check_definition() -> ToolDefinition {
    ToolDefinition {
        name: "memory_context_check".into(),
        description: "Proactive context-aware memory: when context usage exceeds the threshold, auto-compile the conversation into facts, distill long-term memories, and (re)build the user profile. Below threshold it is a read-only diagnostic.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": {"type": "string", "enum": ["user", "assistant", "system", "tool"]},
                            "content": {"type": "string"},
                            "turn_id": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        },
                        "required": ["role", "content"]
                    }
                },
                "context_usage_percent": {"type": "number", "minimum": 0, "maximum": 100, "description": "Current context-window usage percentage reported by the agent host"},
                "threshold": {"type": "number", "description": "Distillation trigger threshold (default 40)"},
                "conversation_id": {"type": "string", "description": "Required when distillation is triggered"},
                "tenant_id": {"type": "string", "default": "default"},
                "user_id": {"type": "string"}
            },
            "required": ["messages", "context_usage_percent"]
        }),
    }
}

/// Aggregate a user's Facts into a structured profile by `FactType`.
///
/// The profile is a deterministic, grouped view of the user's cognitive
/// state: preferences, goals, emotions, identity, occupation. Payload
/// `content` fields carry the human-readable statements.
fn build_user_profile(fact_store: &SqliteFactStore, user_entity_id: i64) -> Result<Value, Error> {
    let all = fact_store.get_facts(user_entity_id)?;

    let mut preferences = Vec::new();
    let mut goals = Vec::new();
    let mut emotions = Vec::new();
    let mut identity = Vec::new();
    let mut occupation = Vec::new();
    let mut others = 0usize;

    for fact in &all {
        let content = fact
            .payload
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match fact.fact_type {
            FactType::Preference => preferences.push(content),
            FactType::Goal => goals.push(content),
            FactType::Emotion => emotions.push(content),
            FactType::Identity => identity.push(content),
            FactType::Occupation => occupation.push(content),
            _ => others += 1,
        }
    }

    // Deterministic ordering: sort each bucket, dedup.
    fn sorted_unique(v: &mut Vec<String>) -> Vec<String> {
        v.sort();
        v.dedup();
        v.clone()
    }

    Ok(json!({
        "entity_id": user_entity_id,
        "preferences": sorted_unique(&mut preferences),
        "goals": sorted_unique(&mut goals),
        "emotions": sorted_unique(&mut emotions),
        "identity": sorted_unique(&mut identity),
        "occupation": sorted_unique(&mut occupation),
        "other_facts": others,
    }))
}

/// Parse raw message JSON values into [`Message`]s, preserving optional ids.
fn parse_messages(values: &[Value]) -> Result<Vec<Message>, Error> {
    values
        .iter()
        .map(|v| {
            let role = v
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("message missing `role`".into()))?;
            let content = v
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("message missing `content`".into()))?
                .to_string();
            let mut message = Message::new(role, content);
            message.turn_id = v.get("turn_id").and_then(Value::as_str).map(String::from);
            message.tool_call_id = v
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(String::from);
            Ok(message)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify the tool stays read-only below the threshold.
    /// Invariants: No facts are persisted; response reports distill_mode=false.
    #[tokio::test]
    async fn below_threshold_is_read_only() {
        let fact_store = Arc::new(
            SqliteFactStore::open_in_memory().expect("isolated fact store for the handler test"),
        );
        let tool = ContextCheckTool::new(None, fact_store.clone());
        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "user_id": "alice",
                "context_usage_percent": 20,
                "messages": [{"role": "user", "content": "I want to learn Rust."}]
            }))
            .await
            .expect("below-threshold call must succeed");
        let payload: Value = serde_json::from_str(
            &result
                .content
                .first()
                .and_then(|block| block.text.clone())
                .unwrap_or_default(),
        )
        .expect("valid JSON");
        assert_eq!(payload["distill_mode"], json!(false), "must stay read-only");
        assert_eq!(
            payload["current"]["facts"],
            json!(0),
            "no facts must be persisted below threshold"
        );
    }

    /// Objective: Verify the tool triggers distillation above the threshold.
    /// Invariants: Facts are persisted, the profile contains the extracted
    /// goal/preference statements from the compiled conversation.
    #[tokio::test]
    async fn above_threshold_compiles_and_builds_profile() {
        let fact_store = Arc::new(
            SqliteFactStore::open_in_memory().expect("isolated fact store for the handler test"),
        );
        let tool = ContextCheckTool::new(None, fact_store.clone());
        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "user_id": "alice",
                "context_usage_percent": 65,
                "threshold": 40,
                "messages": [
                    {"role": "user", "content": "I want to learn Rust."},
                    {"role": "assistant", "content": "Let us start with ownership."}
                ]
            }))
            .await
            .expect("above-threshold call must succeed");
        let payload: Value = serde_json::from_str(
            &result
                .content
                .first()
                .and_then(|block| block.text.clone())
                .unwrap_or_default(),
        )
        .expect("valid JSON");

        assert_eq!(
            payload["distill_mode"],
            json!(true),
            "must trigger distill mode"
        );
        assert!(
            payload["compiled"]["facts_stored"].as_u64().unwrap_or(0) > 0,
            "facts must be persisted when triggered"
        );
        let goals = payload["profile"]["goals"].as_array().expect("goals array");
        assert!(
            goals
                .iter()
                .any(|g| g.as_str().unwrap_or("").contains("learn Rust")),
            "profile goals must include the compiled goal; got {goals:?}"
        );
    }
}
