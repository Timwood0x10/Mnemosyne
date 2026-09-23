//! `state_timeline` MCP tool — the "how did the current state emerge?" layer
//! (cognitive-state plan, Step 2).
//!
//! While `persona_timeline` reconstructs *who a person has become* from persona
//! milestones, `state_timeline` answers the temporal question per cognitive
//! dimension: "what was true before, and how did it change?".
//!
//! It projects an entity's facts through [`StateEngine::aggregate_intervals`]
//! into per-dimension [`StateEvolution`]s: time-ordered [`StateInterval`]s
//! (state validity windows with their evidence anchors) plus deterministic
//! [`StateTransition`]s between them.
//!
//! Determinism guarantees (frozen):
//!
//! - **ADD-only**: facts are never mutated or removed — intervals are a
//!   derived view, always recomputable from facts.
//! - **Allowed to be uncertain**: when no definite relation exists between two
//!   intervals no transition is emitted. We never hallucinate a transition and
//!   never consult an LLM.
//! - `StateInterval.from/to` are *state validity* times, not observation times
//!   (falling back to observation-derived intervals when no valid time exists).
//!
//! Read-only: this tool never writes.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::cognition::{FactStore, StateEngine};
use crate::error::{Error, Result};
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};

/// The five cognitive dimensions `state_timeline` can filter on, mirrored from
/// `crate::state::COGNITIVE_DIMENSIONS` for the tool's `dimension` argument.
pub const STATE_TIMELINE_DIMENSIONS: &[&str] =
    &["goal", "preference", "emotion", "relationship", "identity"];

/// Handler for the `state_timeline` tool.
pub struct StateTimelineTool {
    store: Arc<SqliteFactStore>,
}

impl StateTimelineTool {
    /// Construct the tool over the shared fact store.
    #[must_use]
    pub fn new(store: Arc<SqliteFactStore>) -> Self {
        Self { store }
    }
}

/// Render one interval as a client-friendly object.
fn interval_json(interval: &crate::state::StateInterval) -> Value {
    json!({
        "from": interval.from,
        "to": interval.to,
        "value": interval.value,
        "fact_ids": interval.fact_ids,
        "evidence_ids": interval.evidence_ids,
    })
}

/// Render one transition as a client-friendly object.
fn transition_json(transition: &crate::state::StateTransition) -> Value {
    json!({
        "from_index": transition.from_index,
        "to_index": transition.to_index,
        "at": transition.at,
        "transition_type": transition.transition_type,
        "evidence_ids": transition.evidence_ids,
    })
}

#[async_trait]
impl ToolHandler for StateTimelineTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let entity_id = args
            .get("entity_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::InvalidInput("missing required argument `entity_id`".into()))?;

        let dimension_filter = match args.get("dimension") {
            Some(Value::Null) | None => None,
            Some(value) => {
                let name = value
                    .as_str()
                    .ok_or_else(|| Error::InvalidInput("`dimension` must be a string".into()))?;
                if !STATE_TIMELINE_DIMENSIONS.contains(&name) {
                    return Err(Error::InvalidInput(format!(
                        "unknown dimension `{name}`, expected one of {STATE_TIMELINE_DIMENSIONS:?}"
                    )));
                }
                Some(name.to_string())
            }
        };

        let facts = self.store.get_facts(entity_id)?;
        let engine = StateEngine::new();
        let evolutions = engine.aggregate_intervals(&facts);

        let filtered: Vec<Value> = evolutions
            .iter()
            .filter(|evolution| {
                dimension_filter
                    .as_deref()
                    .map(|name| evolution.key == name)
                    .unwrap_or(true)
            })
            .map(|evolution| {
                json!({
                    "dimension": evolution.key,
                    "intervals": evolution
                        .intervals
                        .iter()
                        .map(interval_json)
                        .collect::<Vec<_>>(),
                    "transitions": evolution
                        .transitions
                        .iter()
                        .map(transition_json)
                        .collect::<Vec<_>>(),
                })
            })
            .collect();

        let payload = json!({
            "entity_id": entity_id,
            "dimensions": filtered,
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Return the stable MCP schema for `state_timeline`.
#[must_use]
pub fn state_timeline_definition() -> ToolDefinition {
    ToolDefinition {
        name: "state_timeline".into(),
        description: "Return how an entity's cognitive state emerged: per-dimension state intervals (when each state was valid, with evidence anchors) and deterministic transitions between them (gradual_change / stance_flip / behavioral_confirmation). ADD-only, read-only, deterministic; a change without a definite signal is reported as intervals only.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "entity_id": {
                    "type": "integer",
                    "description": "The entity id whose state history to reconstruct (required)"
                },
                "dimension": {
                    "type": "string",
                    "enum": STATE_TIMELINE_DIMENSIONS,
                    "description": "Optional: restrict to one cognitive dimension (goal/preference/emotion/relationship/identity)"
                }
            },
            "required": ["entity_id"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::{Fact, FactType};

    fn parse_payload(result: &ToolCallResult) -> Value {
        serde_json::from_str(
            &result
                .content
                .first()
                .and_then(|block| block.text.clone())
                .unwrap_or_default(),
        )
        .expect("valid JSON payload")
    }

    fn fact(id: i64, entity_id: i64, fact_type: FactType, time: i32, payload: Value) -> Fact {
        Fact {
            id: Some(id),
            entity_id,
            fact_type,
            time,
            payload,
            evidence_id: None,
            created_at: i64::from(time),
            ..Fact::default()
        }
    }

    /// Objective: Verify `state_timeline` preserves all three historical states
    /// as intervals (Python → Rust emerging → Rust dominant) and reports the
    /// current interval as open.
    /// Invariants: preference dimension has three intervals; from times ascend;
    /// the latest interval's `to` is null.
    #[tokio::test]
    async fn state_timeline_preserves_historical_states() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity_id = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve user");
        for (id, time, content) in [
            (1i64, 2024i32, "喜欢 Python"),
            (2, 2025, "开始喜欢 Rust"),
            (3, 2026, "主要使用 Rust"),
        ] {
            store
                .insert_fact(&fact(
                    id,
                    entity_id,
                    FactType::Preference,
                    time,
                    json!({"preference": "programming_language", "content": content}),
                ))
                .expect("insert preference fact");
        }

        let tool = StateTimelineTool::new(store);
        let result = tool
            .call(&json!({"entity_id": entity_id, "dimension": "preference"}))
            .await
            .expect("state_timeline succeeds");
        let payload = parse_payload(&result);
        let dimensions = payload["dimensions"].as_array().expect("dimensions array");
        assert_eq!(dimensions.len(), 1, "filtered to the preference dimension");
        let intervals = dimensions[0]["intervals"]
            .as_array()
            .expect("intervals array");
        assert_eq!(intervals.len(), 3, "all three historical states preserved");
        assert_eq!(intervals[0]["from"], json!(2024));
        assert_eq!(intervals[1]["from"], json!(2025));
        assert_eq!(intervals[2]["from"], json!(2026));
        assert_eq!(
            intervals[0]["to"],
            json!(2025),
            "first interval closes at 2025"
        );
        assert_eq!(
            intervals[1]["to"],
            json!(2026),
            "second interval closes at 2026"
        );
        assert!(
            intervals[2]["to"].is_null(),
            "latest interval stays open (current state)"
        );
        assert_eq!(
            dimensions[0]["dimension"],
            json!("preference"),
            "dimension key is reported"
        );
    }

    /// Objective: Verify a definite stance flip is reported as a StanceFlip
    /// transition, while a change without a definite signal yields intervals
    /// only (never a fabricated transition).
    /// Invariants: stance-flip case → one transition of type stance_flip;
    /// unrelated change case → transitions empty.
    #[tokio::test]
    async fn state_timeline_detects_definite_transitions_only() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity_id = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve user");

        // A definite stance flip: same topic, opposite negation.
        store
            .insert_fact(&fact(
                1,
                entity_id,
                FactType::Preference,
                2024,
                json!({"preference": "应酬", "content": "我喜欢应酬", "negated": false}),
            ))
            .expect("insert pre-flip fact");
        store
            .insert_fact(&fact(
                2,
                entity_id,
                FactType::Preference,
                2026,
                json!({"preference": "应酬", "content": "我不喜欢应酬", "negated": true}),
            ))
            .expect("insert post-flip fact");

        let tool = StateTimelineTool::new(store.clone());
        let result = tool
            .call(&json!({"entity_id": entity_id}))
            .await
            .expect("state_timeline succeeds");
        let payload = parse_payload(&result);
        let dimensions = payload["dimensions"].as_array().expect("dimensions array");
        let preference = dimensions
            .iter()
            .find(|d| d["dimension"] == "preference")
            .expect("preference dimension present");
        let transitions = preference["transitions"]
            .as_array()
            .expect("transitions array");
        assert_eq!(transitions.len(), 1, "the definite stance flip is detected");
        assert_eq!(
            transitions[0]["transition_type"],
            json!("stance_flip"),
            "transition type is stance_flip"
        );

        // An unrelated change (no shared topic) must NOT fabricate a transition.
        let store2 = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity2 = store2.resolve_user("tenant-a", "bob").expect("resolve bob");
        store2
            .insert_fact(&fact(
                1,
                entity2,
                FactType::Preference,
                2024,
                json!({"preference": "应酬", "content": "我讨厌应酬", "negated": true}),
            ))
            .expect("insert fact A");
        store2
            .insert_fact(&fact(
                2,
                entity2,
                FactType::Preference,
                2026,
                json!({"preference": "安稳", "content": "我喜欢安稳", "negated": false}),
            ))
            .expect("insert fact B");
        let tool2 = StateTimelineTool::new(store2);
        let result2 = tool2
            .call(&json!({"entity_id": entity2}))
            .await
            .expect("state_timeline succeeds");
        let payload2 = parse_payload(&result2);
        let dimensions2 = payload2["dimensions"].as_array().expect("dimensions array");
        let preference2 = dimensions2
            .iter()
            .find(|d| d["dimension"] == "preference")
            .expect("preference dimension present");
        assert!(
            preference2["transitions"]
                .as_array()
                .map(|t| t.is_empty())
                .unwrap_or(true),
            "unrelated change must not fabricate a transition"
        );
        assert_eq!(
            preference2["intervals"].as_array().map(|i| i.len()),
            Some(2),
            "both states are still preserved as intervals"
        );
    }

    /// Objective: Verify input validation — missing entity_id, unknown
    /// dimension, and an empty entity all behave cleanly.
    /// Invariants: missing entity_id → InvalidInput; unknown dimension →
    /// InvalidInput; empty entity → empty dimensions, no error.
    #[tokio::test]
    async fn state_timeline_validates_inputs() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let tool = StateTimelineTool::new(store.clone());

        let error = tool
            .call(&json!({}))
            .await
            .expect_err("missing entity_id must fail");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "missing entity_id is InvalidInput, got {error:?}"
        );

        let error = tool
            .call(&json!({"entity_id": 7, "dimension": "nonsense"}))
            .await
            .expect_err("unknown dimension must fail");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "unknown dimension is InvalidInput, got {error:?}"
        );

        // An entity with no facts yields empty dimensions, not an error.
        let entity_id = store
            .resolve_user("tenant-a", "nobody")
            .expect("resolve empty user");
        let result = tool
            .call(&json!({"entity_id": entity_id}))
            .await
            .expect("empty entity is not an error");
        let payload = parse_payload(&result);
        assert_eq!(
            payload["dimensions"].as_array().map(|d| d.len()),
            Some(0),
            "no facts → no dimensions"
        );
    }

    /// Objective: Verify the tool schema declares the required entity_id and
    /// the dimension enum.
    /// Invariants: required contains entity_id; dimension enum matches the
    /// five cognitive dimensions.
    #[test]
    fn state_timeline_definition_declares_contract() {
        let definition = state_timeline_definition();
        assert_eq!(definition.name, "state_timeline");
        let required: Vec<&str> = definition
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(required.contains(&"entity_id"), "entity_id is required");
        let enum_values: Vec<&str> = definition
            .input_schema
            .pointer("/properties/dimension/enum")
            .and_then(Value::as_array)
            .expect("dimension enum array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(enum_values, STATE_TIMELINE_DIMENSIONS);
    }
}
