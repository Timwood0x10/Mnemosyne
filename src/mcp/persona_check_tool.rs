//! `persona_check` MCP tool — the "人设不崩" consistency guard.
//!
//! Given an agent's draft reply and the accumulated persona facts for that
//! agent entity, the tool reports whether the draft:
//!
//! - **contradicts** an established persona fact ([`PersonaConflict`]) — same
//!   `fact_type`, opposite `negated`, high similarity; or
//! - **drifts** from the persona ([`PersonaDrift`]) by introducing a statement
//!   with no anchor in the stored persona.
//!
//! No LLM is involved: the semantic path embeds the draft and each stored
//! persona fact and compares cosine similarity (see [`crate::persona::check`]).
//! When the embedding backend or the prototype config is unavailable, the tool
//! transparently falls back to the offline keyword path so the guard stays
//! usable. This is a **check-only** tool — it never writes facts.
//!
//! The persona facts participating in the comparison are those tagged
//! `attribution == "agent_personality"` for the resolved agent entity.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::cognition::FactStore;
use crate::config::resolve_resource_path;
use crate::embed::EmbeddingService;
use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};
use crate::persona::check::{PersonaConflict, PersonaDrift};
use crate::persona::prototype::{PersonaThresholds, PrototypeVectorCache, load_prototype_config};
use crate::persona::{PersonaCheckEngine, PersonaSignal};

/// Handler for the `persona_check` tool.
pub struct PersonaCheckTool {
    fact_store: Arc<SqliteFactStore>,
    engine: PersonaCheckEngine,
}

impl PersonaCheckTool {
    /// Construct the tool with the shared fact store and embedder.
    ///
    /// The semantic path is enabled when the embedder is on **and** the
    /// prototype config loads successfully; otherwise the engine falls back to
    /// the offline keyword path. A missing or malformed prototype config is
    /// logged and never fails the whole server — the guard still works.
    pub async fn new(
        fact_store: Arc<SqliteFactStore>,
        embedder: Arc<dyn EmbeddingService>,
    ) -> Self {
        let path = resolve_resource_path("config/persona_prototypes.json");
        let (cache, thresholds) = match load_prototype_config(&path) {
            Ok(cfg) => {
                let thresholds = cfg.thresholds;
                if embedder.enabled() {
                    match PrototypeVectorCache::build(&cfg, embedder.as_ref()).await {
                        Ok(c) => {
                            tracing::info!("persona_check using semantic path");
                            (Some(c), thresholds)
                        }
                        Err(e) => {
                            tracing::warn!(
                                "persona prototype cache build failed; using keyword fallback: {e}"
                            );
                            (None, thresholds)
                        }
                    }
                } else {
                    tracing::info!("persona_check embedder disabled; using keyword fallback");
                    (None, thresholds)
                }
            }
            Err(e) => {
                tracing::warn!("persona prototype config load failed; using keyword fallback: {e}");
                (None, PersonaThresholds::default())
            }
        };
        let engine = PersonaCheckEngine::new(cache, embedder, thresholds);
        Self { fact_store, engine }
    }
}

#[async_trait]
impl ToolHandler for PersonaCheckTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let agent_id = args
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `agent_id`".into()))?;
        let draft = args
            .get("draft")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `draft`".into()))?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");

        // Resolve the agent entity (stable per tenant/agent pair) and read its
        // accumulated facts. Only `agent_personality`-tagged facts participate.
        let entity_id = self.fact_store.resolve_agent(tenant_id, agent_id)?;
        let facts = self.fact_store.get_facts(entity_id)?;
        let result = self.engine.check(draft, &facts).await?;

        let payload = json!({
            "clean": result.is_clean(),
            "conflicts": serialize_conflicts(&result.conflicts),
            "drift": serialize_drift(&result.drift),
            "stats": {
                "consistent_count": result.consistent_count,
                "total_signals": result.total_signals,
            },
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Serialize the conflict list into a client-friendly JSON array.
fn serialize_conflicts(conflicts: &[PersonaConflict]) -> Vec<Value> {
    conflicts
        .iter()
        .map(|c| {
            json!({
                "fact_type": c.fact_type,
                "draft_signal": serialize_signal(&c.draft_signal),
                "stored_fact_id": c.stored_fact_id,
                "stored_content": c.stored_content,
                "stored_negated": c.stored_negated,
                "similarity": c.similarity,
            })
        })
        .collect()
}

/// Serialize the drift list into a client-friendly JSON array.
fn serialize_drift(drift: &[PersonaDrift]) -> Vec<Value> {
    drift
        .iter()
        .map(|d| {
            json!({
                "fact_type": d.fact_type,
                "draft_signal": serialize_signal(&d.draft_signal),
                "reason": d.reason,
            })
        })
        .collect()
}

/// Serialize a single persona signal into a client-friendly JSON object.
fn serialize_signal(signal: &PersonaSignal) -> Value {
    json!({
        "text": signal.text,
        "fact_type": signal.fact_type,
        "negated": signal.negated,
        "confidence": signal.confidence,
    })
}

/// Return the stable MCP schema for `persona_check`.
pub fn persona_check_definition() -> ToolDefinition {
    ToolDefinition {
        name: "persona_check".into(),
        description: "Guard against persona inconsistency (人设不崩): check an agent's draft reply against the accumulated persona facts and report contradictions (conflicts) and unanchored statements (drift). No LLM — embedding semantic match with keyword fallback. Read-only.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "agent_id": {"type": "string", "description": "Agent/entity identifier whose stored persona facts are compared against the draft"},
                "draft": {"type": "string", "description": "The agent's draft reply text to check for persona consistency"},
                "tenant_id": {"type": "string", "default": "default", "description": "Tenant namespace for the agent entity"}
            },
            "required": ["agent_id", "draft"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_personality::AGENT_PERSONALITY_ATTRIBUTION;
    use crate::cognition::{Fact, FactType};
    use crate::embed::NullEmbedder;

    /// Seed a persona fact for an agent entity through the real fact store.
    fn seed_persona_fact(
        store: &SqliteFactStore,
        entity_id: i64,
        fact_type: FactType,
        negated: bool,
        content: &str,
    ) {
        store
            .insert_fact(&Fact {
                id: None,
                entity_id,
                fact_type,
                time: 1,
                payload: json!({
                    "attribution": AGENT_PERSONALITY_ATTRIBUTION,
                    "content": content,
                    "negated": negated,
                }),
                evidence_id: None,
                created_at: 1,
                ..Fact::default()
            })
            .expect("insert persona fact");
    }

    /// Build a tool over the keyword path (NullEmbedder → cache=None), which
    /// is deterministic without an embedding backend.
    async fn keyword_tool(store: Arc<SqliteFactStore>) -> PersonaCheckTool {
        PersonaCheckTool::new(store, Arc::new(NullEmbedder)).await
    }

    /// Parse the tool call's text payload into a JSON value.
    fn parse_payload(result: &ToolCallResult) -> Value {
        serde_json::from_str(
            &result
                .content
                .first()
                .and_then(|b| b.text.clone())
                .unwrap_or_default(),
        )
        .expect("valid JSON payload")
    }

    /// Objective: Verify the tool flags a draft that contradicts a stored
    /// persona fact (opposite negated, shared-bigram overlap) as a conflict.
    /// Invariants: stored "我喜欢应酬" (negated=false) + draft "我讨厌应酬" →
    /// one conflict, clean=false, no drift.
    #[tokio::test]
    async fn contradictory_draft_reports_conflict() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity_id = store
            .resolve_agent("tenant-a", "agent-bailiusu")
            .expect("resolve agent");
        seed_persona_fact(&store, entity_id, FactType::Preference, false, "我喜欢应酬");
        let tool = keyword_tool(store).await;

        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "agent_id": "agent-bailiusu",
                "draft": "我讨厌应酬，太累了。"
            }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);
        assert_eq!(payload["clean"], json!(false), "must not be clean");
        let conflicts = payload["conflicts"].as_array().expect("conflicts array");
        assert_eq!(conflicts.len(), 1, "one conflict reported");
        assert_eq!(conflicts[0]["fact_type"], json!("Preference"));
        assert!(
            conflicts[0]["draft_signal"]["negated"]
                .as_bool()
                .unwrap_or(false),
            "draft signal flagged as negated"
        );
        assert_eq!(payload["drift"].as_array().expect("drift array").len(), 0);
    }

    /// Objective: Verify a draft that repeats an established persona fact is
    /// reported clean (no conflict, no drift).
    /// Invariants: stored "我喜欢安稳" + draft "我喜欢安稳踏实" → clean=true.
    #[tokio::test]
    async fn consistent_draft_is_clean() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity_id = store
            .resolve_agent("tenant-a", "agent-bailiusu")
            .expect("resolve agent");
        seed_persona_fact(&store, entity_id, FactType::Preference, false, "我喜欢安稳");
        let tool = keyword_tool(store).await;

        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "agent_id": "agent-bailiusu",
                "draft": "我喜欢安稳踏实。"
            }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);
        assert_eq!(payload["clean"], json!(true), "repeat must be clean");
        assert_eq!(payload["conflicts"].as_array().expect("conflicts").len(), 0);
        assert_eq!(payload["drift"].as_array().expect("drift").len(), 0);
    }

    /// Objective: Verify a persona statement with no anchor in the stored
    /// persona is reported as drift.
    /// Invariants: draft "我是白流苏" but stored has only a Preference fact →
    /// one drift (no stored fact of this type), clean=false.
    #[tokio::test]
    async fn unanchored_statement_is_drift() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity_id = store
            .resolve_agent("tenant-a", "agent-bailiusu")
            .expect("resolve agent");
        seed_persona_fact(&store, entity_id, FactType::Preference, false, "我喜欢安稳");
        let tool = keyword_tool(store).await;

        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "agent_id": "agent-bailiusu",
                "draft": "我是白流苏，离过婚。"
            }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);
        assert_eq!(
            payload["clean"],
            json!(false),
            "unanchored must not be clean"
        );
        let drift = payload["drift"].as_array().expect("drift array");
        assert_eq!(drift.len(), 1, "one drift reported");
        assert_eq!(drift[0]["fact_type"], json!("Identity"));
    }

    /// Objective: Verify a draft with no persona signal yields a clean result.
    /// Invariants: "今天天气不错。" → clean=true, total_signals=0.
    #[tokio::test]
    async fn no_signal_is_clean() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity_id = store
            .resolve_agent("tenant-a", "agent-bailiusu")
            .expect("resolve agent");
        seed_persona_fact(&store, entity_id, FactType::Preference, false, "我喜欢安稳");
        let tool = keyword_tool(store).await;

        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "agent_id": "agent-bailiusu",
                "draft": "今天天气不错。"
            }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);
        assert_eq!(payload["clean"], json!(true), "no signal → clean");
        assert_eq!(payload["stats"]["total_signals"], json!(0));
    }

    /// Objective: Verify the tool rejects a call missing the required
    /// `draft` argument with an InvalidInput error.
    /// Invariants: calling with only `agent_id` → Err(InvalidInput).
    #[tokio::test]
    async fn missing_draft_is_invalid_input() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let tool = keyword_tool(store).await;
        let err = tool
            .call(&json!({ "agent_id": "agent-bailiusu" }))
            .await
            .expect_err("missing draft must error");
        assert!(
            matches!(err, Error::InvalidInput(_)),
            "expected InvalidInput, got {err:?}"
        );
        assert!(err.to_string().contains("draft"));
    }

    /// Objective: Verify the tool schema requires `agent_id` and `draft`.
    /// Invariants: the definition's required array contains both.
    #[test]
    fn definition_requires_agent_id_and_draft() {
        let def = persona_check_definition();
        let required = def
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required array");
        let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert!(names.contains(&"agent_id"), "schema requires agent_id");
        assert!(names.contains(&"draft"), "schema requires draft");
        assert_eq!(def.name, "persona_check");
    }
}
