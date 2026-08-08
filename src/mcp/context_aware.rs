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
use crate::config::{CONTEXT_INJECT_THRESHOLD, EMBEDDING_MEMORY_GRAYSCALE};
use crate::distiller::{Distiller, PipelineDistiller};
use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};
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
    compiler: CognitionCompiler,
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
            compiler: CognitionCompiler::new(),
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

        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let user_id = args.get("user_id").and_then(Value::as_str).unwrap_or("");

        let context_usage = Self::parse_context_usage(args);
        let threshold = Self::parse_threshold(args);
        let triggered = context_usage >= threshold;

        if !triggered {
            // No-op diagnostic: report usage and current memory state.
            // Grayscale OFF → exact legacy payload (no inject_memories field,
            // original reason text). Grayscale ON → empty injection slot
            // (plan P3: below the gate we inject nothing, preserving the
            // host's context window).
            //
            // Read-only resolve: a below-threshold diagnostic must NOT
            // materialize an entity for a user who never chatted (audit:
            // read-only tools writing via resolve_user). find_entity never
            // writes; unknown users simply report zero facts.
            let norm_tenant = if tenant_id.trim().is_empty() {
                "default"
            } else {
                tenant_id.trim()
            };
            let norm_user = if user_id.trim().is_empty() {
                "default"
            } else {
                user_id.trim()
            };
            let name = if norm_user == "default" {
                "User".to_string()
            } else {
                format!("User:{norm_user}")
            };
            let user_entity_id = self
                .fact_store
                .find_entity(norm_tenant, Some(norm_user), &name)?
                .map(|(id, _, _)| id);
            let facts = match user_entity_id {
                Some(id) => self.fact_store.get_facts(id)?,
                None => Vec::new(),
            };
            let mut payload = serde_json::Map::new();
            payload.insert("context_usage_percent".into(), json!(context_usage));
            payload.insert("threshold_percent".into(), json!(threshold));
            payload.insert("distill_mode".into(), json!(false));
            payload.insert(
                "reason".into(),
                json!(format!(
                    "context usage {context_usage:.0}% below threshold {threshold:.0}% — no distillation{}",
                    if EMBEDDING_MEMORY_GRAYSCALE { ", no injection" } else { "" }
                )),
            );
            payload.insert(
                "current".into(),
                json!({
                    "user_entity_id": user_entity_id,
                    "facts": facts.len(),
                }),
            );
            if EMBEDDING_MEMORY_GRAYSCALE {
                payload.insert("inject_memories".into(), json!([]));
            }
            return Ok(ToolCallResult::text(
                serde_json::Value::Object(payload).to_string(),
            ));
        }

        // ── Triggered: compile → persist → distill → profile ────────────
        let user_entity_id = self.fact_store.resolve_user(tenant_id, user_id)?;
        let logical_time = chrono::Utc::now().timestamp() as i32;
        let compiled =
            self.compiler
                .compile_conversation(tenant_id, &messages, user_entity_id, logical_time);
        let stored_facts = self.fact_store.insert_batch(&compiled.facts)?;

        // Distill the conversation into long-term memories (knowledge/preferences…).
        // Grayscale ON: the distilled memories ARE the injection payload —
        // original text, never an LLM rewrite ("检索代数化、注入原文化").
        let mut distilled = 0usize;
        let mut inject_memories: Vec<String> = Vec::new();
        if let Some(distiller) = &self.distiller {
            let conversation_id = args
                .get("conversation_id")
                .and_then(Value::as_str)
                .unwrap_or("context-check");
            let memories = distiller
                .distill(conversation_id, &messages, tenant_id, user_id)
                .await?;
            distilled = memories.len();
            if EMBEDDING_MEMORY_GRAYSCALE {
                inject_memories = memories
                    .iter()
                    .map(|m| m.content.clone())
                    .filter(|c| !c.is_empty())
                    .collect();
            }
        }

        // Rebuild the user profile from ALL accumulated facts of this user.
        let profile = build_user_profile(&self.fact_store, user_entity_id)?;

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
                "observations": compiled.observations.len(),
                "facts_compiled": compiled.facts.len(),
                "facts_stored": stored_facts,
                "distilled_memories": distilled,
            }),
        );
        payload.insert("profile".into(), profile);
        if EMBEDDING_MEMORY_GRAYSCALE {
            payload.insert("inject_memories".into(), json!(inject_memories));
        }
        Ok(ToolCallResult::text(
            serde_json::Value::Object(payload).to_string(),
        ))
    }
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
