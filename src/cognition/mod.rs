//! Cognition Engine — core types for the unified cognitive compiler.
//!
//! This module defines the cross-cutting types that replace ad-hoc structures
//! from Mnemosyne and Memory Distillation with a unified data model.
//!
//! ## Existing infrastructure reused
//!
//! | New type          | Reuses                        | Module                    |
//! |-------------------|-------------------------------|---------------------------|
//! | `Mention`         | `compiler::Mention` (re-export)| `src/compiler/mod.rs`     |
//! | `Entity`          | `compiler::Entity` (re-export) | `src/compiler/mod.rs`     |
//! | `EvidenceWriter`  | `compiler::writer`            | `src/compiler/writer.rs`  |
//! | `LanguageFrontend`| `language::LanguageProvider`   | `src/language.rs`         |

use serde::{Deserialize, Serialize};

use crate::error::Result;

// ═══════════════════════════════════════════════════════════════════════════
// Observation (Compiler IR)
// ═══════════════════════════════════════════════════════════════════════════

/// A resolved entity mention — reuses the same concept as compiler::Mention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mention {
    pub entity_id: Option<i64>,
    pub surface: String,
    pub canonical_name: String,
}

/// A single observed action — the universal IR.
///
/// ```text
/// "Anna loved Vronsky"
/// → Observation { subject: Anna, action: "love", object: Vronsky }
///
/// "最近压力很大"
/// → Observation { subject: User, action: "feel", object: "stress" }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub subject: Mention,
    pub action: String,
    pub object: Option<Mention>,
    pub modifiers: Vec<Modifier>,
    pub timestamp: Option<i32>,
    pub evidence: Option<EvidenceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Modifier {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub doc_id: i64,
    pub offset: usize,
    pub length: usize,
    pub text: String,
}

// ═══════════════════════════════════════════════════════════════════════════
// Fact — the only stored unit (Event Sourcing)
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FactType {
    Identity,
    Preference,
    Goal,
    Event,
    Relationship,
    Emotion,
    Location,
    Occupation,
    Interest,
    Habit,
}

impl FactType {
    /// Stable lowercase name for this type.
    ///
    /// Used both as the stored `fact_type` column value and as the semantic key
    /// of a cognitive dimension, so the storage layer and the state-history
    /// layer can never disagree on how a dimension is spelled.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FactType::Identity => "identity",
            FactType::Preference => "preference",
            FactType::Goal => "goal",
            FactType::Event => "event",
            FactType::Relationship => "relationship",
            FactType::Emotion => "emotion",
            FactType::Location => "location",
            FactType::Occupation => "occupation",
            FactType::Interest => "interest",
            FactType::Habit => "habit",
        }
    }
}

/// Epistemic life-cycle status of a fact (v0.3 cognitive-state upgrade).
///
/// Strictly three states — do NOT extend (no Expired/Archived/Pending/...).
/// `status` is orthogonal to `confidence` (epistemic confidence) and to decay:
/// it answers "is this claim still believed to hold?", not "how confident are
/// we?" nor "how stale is it?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    /// Currently still believed to hold.
    #[default]
    Active,
    /// Superseded by a newer state (e.g. Python preference → Rust preference).
    /// The old claim was once true but is now replaced.
    Superseded,
    /// In explicit conflict, but the evidence cannot decide which is true
    /// (e.g. 喜欢独处 vs 喜欢热闹). Do NOT silently resolve by "newest wins".
    Contradicted,
}

impl FactStatus {
    /// Stable lowercase string stored in the `status` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FactStatus::Active => "active",
            FactStatus::Superseded => "superseded",
            FactStatus::Contradicted => "contradicted",
        }
    }

    /// Parse the stored string back into a [`FactStatus`].
    ///
    /// Unknown or missing values fall back to [`FactStatus::Active`] so legacy
    /// rows (which have no status column value) migrate cleanly.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "superseded" => FactStatus::Superseded,
            "contradicted" => FactStatus::Contradicted,
            _ => FactStatus::Active,
        }
    }
}

fn default_confidence() -> f64 {
    1.0
}

/// A single atomic fact. Immutable once written.
/// State is derived from facts via aggregation.
///
/// ## v0.3 provenance upgrade
///
/// A fact is the **evidence unit** of cognitive state, not the final product:
///
/// - `evidence_id` — the original-text evidence that justifies the fact
///   (Why do we believe this?).
/// - `confidence` — epistemic confidence, mapped from the legacy `weight`
///   column (decay down-weights it over time).
/// - `derived_from` — the fact ids this fact was derived from. This is a
///   **derivation / provenance chain**, NOT causality: `F2 derived_from F1`
///   means "F2 was inferred from F1", never "F1 caused F2". Causal claims
///   (`causes`/`caused_by`) are deliberately out of scope for v0.3.
/// - `status` — epistemic life-cycle (Active/Superseded/Contradicted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub id: Option<i64>,
    pub entity_id: i64,
    pub fact_type: FactType,
    pub time: i32,
    pub payload: serde_json::Value,
    pub evidence_id: Option<i64>,
    pub created_at: i64,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    #[serde(default)]
    pub derived_from: Vec<i64>,
    #[serde(default)]
    pub status: FactStatus,
}

impl Default for Fact {
    fn default() -> Self {
        Fact {
            id: None,
            entity_id: 0,
            fact_type: FactType::Event,
            time: 0,
            payload: serde_json::json!({}),
            evidence_id: None,
            created_at: 0,
            confidence: 1.0,
            derived_from: Vec::new(),
            status: FactStatus::Active,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Rule trait (Observation → Fact)
// ═══════════════════════════════════════════════════════════════════════════

/// Transforms an Observation into zero or more Facts.
pub trait Rule: Send + Sync {
    fn apply(&self, observation: &Observation) -> Vec<Fact>;
}

mod snapshot;
mod state;

pub use snapshot::{CognitiveContext, EntitySnapshot, build_context, build_snapshot};
pub use state::{EmotionAggregator, EntityState, StateAggregator, StateEngine};

// ═══════════════════════════════════════════════════════════════════════════
// FactStore trait (persistence)
// ═══════════════════════════════════════════════════════════════════════════

/// Storage for Facts — the only write path in the Knowledge Store.
///
/// V1 implementation wraps SQLite with batch inserts (reusing EvidenceWriter's
/// transaction pattern). Future implementations can switch to LanceDB.
pub trait FactStore: Send + Sync {
    fn insert_fact(&self, fact: &Fact) -> Result<i64>;
    fn insert_batch(&self, facts: &[Fact]) -> Result<usize>;
    fn get_facts(&self, entity_id: i64) -> Result<Vec<Fact>>;
    fn get_facts_by_type(&self, entity_id: i64, fact_type: FactType) -> Result<Vec<Fact>>;
    fn get_timeline(&self, entity_id: i64) -> Result<Vec<Fact>>;
    /// Fetch a single fact by its stable id, or `None` when it does not exist.
    fn get_fact_by_id(&self, fact_id: i64) -> Result<Option<Fact>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(fact_type: FactType, time: i32, payload: serde_json::Value) -> Fact {
        Fact {
            id: Some(i64::from(time)),
            entity_id: 7,
            fact_type,
            time,
            payload,
            evidence_id: None,
            created_at: i64::from(time),
            ..Fact::default()
        }
    }

    /// Objective: Verify core state is rebuilt without optional aggregators.
    /// Invariants: Latest goal wins per semantic key and events are newest first.
    #[test]
    fn state_engine_rebuilds_core_state_without_extensions() {
        let facts = vec![
            fact(
                FactType::Goal,
                2024,
                serde_json::json!({"goal": "learn Rust"}),
            ),
            fact(
                FactType::Goal,
                2026,
                serde_json::json!({"goal": "learn Rust", "status": "active"}),
            ),
            fact(
                FactType::Event,
                2025,
                serde_json::json!({"action": "started"}),
            ),
            fact(
                FactType::Event,
                2026,
                serde_json::json!({"action": "shipped"}),
            ),
        ];

        let state = StateEngine::new().aggregate(&facts);

        assert_eq!(
            state.goals.len(),
            1,
            "State should keep one current fact per goal"
        );
        assert_eq!(
            state.goals[0].time, 2026,
            "The newest goal fact should define current state"
        );
        assert_eq!(
            state.recent_events.len(),
            2,
            "All available recent events should be preserved"
        );
        assert_eq!(
            state.recent_events[0].time, 2026,
            "Recent events should be ordered newest first"
        );
        assert!(
            state.extensions.is_empty(),
            "No extension values should exist without custom aggregators"
        );
    }

    /// Objective: Verify snapshot formatters preserve identity and fact evidence.
    /// Invariants: Markdown, embedding text, prompt, and JSON contain stable entity data.
    #[test]
    fn snapshot_formatters_preserve_evidence_backed_content() {
        let snapshot = build_snapshot(
            7,
            "Alice".to_string(),
            "User".to_string(),
            vec![fact(
                FactType::Preference,
                2026,
                serde_json::json!({"topic": "Rust", "score": 0.9}),
            )],
            &StateEngine::new(),
        );

        assert!(
            snapshot.format_markdown().contains("Alice"),
            "Markdown should contain the entity name"
        );
        assert!(
            snapshot.format_embedding().contains("Rust"),
            "Embedding text should contain fact payloads"
        );
        assert!(
            snapshot.format_prompt().contains("Do not invent facts"),
            "Prompt should enforce evidence grounding"
        );
        assert_eq!(
            snapshot.format_json()["entity_id"],
            7,
            "JSON should preserve the entity id"
        );
    }

    /// Objective: Verify facts WITHOUT a semantic payload key are NOT folded
    /// into a single "default" bucket — each distinct fact survives (the
    /// observation-compiler output `{action, subject, object}` has no key, so
    /// previously N distinct goals collapsed into one, silently dropping all
    /// but the last).
    /// Invariants: two distinct unkeyed Goal facts both appear in `goals`.
    #[test]
    fn unkeyed_facts_are_not_folded_together() {
        let facts = vec![
            fact(
                FactType::Goal,
                2024,
                serde_json::json!({"action": "学习", "subject": "Alice", "object": "Rust"}),
            ),
            fact(
                FactType::Goal,
                2026,
                serde_json::json!({"action": "减肥", "subject": "Alice", "object": null}),
            ),
        ];
        let state = StateEngine::new().aggregate(&facts);
        assert_eq!(
            state.goals.len(),
            2,
            "both unkeyed goals must survive, got {}: {:?}",
            state.goals.len(),
            state.goals
        );
    }

    /// Objective: Verify the five-dimension cognitive model is complete —
    /// relationships and identity attributes are aggregated per semantic key
    /// (latest fact wins), just like goals and preferences.
    /// Invariants: latest relationship per target; latest identity per key.
    #[test]
    fn aggregates_relationships_and_identity_dimensions() {
        let facts = vec![
            // Two relationship facts for the same target — newest should win.
            fact(
                FactType::Relationship,
                2024,
                serde_json::json!({"target": "Bob", "kind": "acquaintance"}),
            ),
            fact(
                FactType::Relationship,
                2026,
                serde_json::json!({"target": "Bob", "kind": "colleague"}),
            ),
            fact(
                FactType::Relationship,
                2025,
                serde_json::json!({"target": "Carol", "kind": "friend"}),
            ),
            // Two identity attributes for the same key — newest should win.
            fact(
                FactType::Identity,
                2023,
                serde_json::json!({"attribute": "occupation", "value": "student"}),
            ),
            fact(
                FactType::Identity,
                2026,
                serde_json::json!({"attribute": "occupation", "value": "engineer"}),
            ),
            fact(
                FactType::Identity,
                2025,
                serde_json::json!({"attribute": "nationality", "value": "CN"}),
            ),
        ];

        let state = StateEngine::new().aggregate(&facts);

        // Relationships: one per distinct target (Bob, Carol), Bob uses newest.
        assert_eq!(
            state.relationships.len(),
            2,
            "one relationship fact per target, got {}",
            state.relationships.len()
        );
        let bob = state
            .relationships
            .iter()
            .find(|f| f.payload["target"] == "Bob")
            .expect("Bob relationship present");
        assert_eq!(
            bob.payload["kind"], "colleague",
            "newest relationship fact for Bob wins"
        );

        // Identity: one per distinct attribute, occupation uses newest.
        assert_eq!(
            state.identity_attributes.len(),
            2,
            "one identity fact per attribute, got {}",
            state.identity_attributes.len()
        );
        let occ = state
            .identity_attributes
            .iter()
            .find(|f| f.payload["attribute"] == "occupation")
            .expect("occupation identity present");
        assert_eq!(
            occ.payload["value"], "engineer",
            "newest identity fact for occupation wins"
        );
    }

    /// Objective: Verify `StateEngine::aggregate_intervals` answers "how did
    /// the current state emerge" — it preserves historical states as intervals
    /// across all five cognitive dimensions, while `aggregate()` still returns
    /// only the latest state.
    /// Invariants: aggregate() has exactly one preference; aggregate_intervals()
    /// has three intervals (2024 → 2025 → 2026); the current interval is open
    /// (to == None); no transition is fabricated when states merely differ.
    #[test]
    fn aggregate_intervals_preserves_history_while_aggregate_keeps_latest() {
        let facts = vec![
            fact(
                FactType::Preference,
                2024,
                serde_json::json!({"preference": "programming_language", "content": "喜欢 Python"}),
            ),
            fact(
                FactType::Preference,
                2025,
                serde_json::json!({"preference": "programming_language", "content": "开始喜欢 Rust"}),
            ),
            fact(
                FactType::Preference,
                2026,
                serde_json::json!({"preference": "programming_language", "content": "主要使用 Rust"}),
            ),
        ];

        let engine = StateEngine::new();
        let current = engine.aggregate(&facts);
        assert_eq!(
            current.preferences.len(),
            1,
            "aggregate() keeps only the latest preference"
        );
        assert_eq!(
            current.preferences[0].payload["content"], "主要使用 Rust",
            "aggregate() reports the current state"
        );

        let intervals = engine.aggregate_intervals(&facts);
        let preference_evolution = intervals
            .iter()
            .find(|evolution| evolution.key == "preference")
            .expect("preference dimension present in intervals");
        assert_eq!(
            preference_evolution.intervals.len(),
            3,
            "aggregate_intervals() preserves all three historical states"
        );
        assert_eq!(
            preference_evolution.intervals[0].from, 2024,
            "first interval starts at the earliest state"
        );
        assert_eq!(
            preference_evolution.intervals[1].from, 2025,
            "second interval starts when the state changed"
        );
        assert_eq!(
            preference_evolution.intervals[2].to, None,
            "latest interval is still open (current state)"
        );
        // No negated/keyword signal, no action word: transitions stay empty —
        // the change is reported as intervals only (allowed to be uncertain).
        assert!(
            preference_evolution.transitions.is_empty(),
            "must not fabricate a transition without a definite signal"
        );
    }

    /// Objective: Verify `aggregate_intervals` never mutates facts and
    /// reports a stance flip as a deterministic StanceFlip transition while
    /// keeping both intervals (ADD-only).
    /// Invariants: two intervals survive; exactly one StanceFlip transition
    /// connects them.
    #[test]
    fn aggregate_intervals_detects_stance_flip_deterministically() {
        let facts = vec![
            fact(
                FactType::Preference,
                2024,
                serde_json::json!({
                    "preference": "应酬",
                    "content": "我喜欢应酬",
                    "negated": false,
                }),
            ),
            fact(
                FactType::Preference,
                2026,
                serde_json::json!({
                    "preference": "应酬",
                    "content": "我不喜欢应酬",
                    "negated": true,
                }),
            ),
        ];

        let intervals = StateEngine::new().aggregate_intervals(&facts);
        let evolution = intervals
            .iter()
            .find(|evolution| evolution.key == "preference")
            .expect("preference dimension present");
        assert_eq!(
            evolution.intervals.len(),
            2,
            "ADD-only: both states survive as intervals"
        );
        assert_eq!(
            evolution.transitions.len(),
            1,
            "a definite stance flip is detected"
        );
        assert_eq!(
            evolution.transitions[0].transition_type,
            crate::state::TransitionType::StanceFlip,
            "the transition is a stance flip"
        );
        assert_eq!(
            evolution.transitions[0].at, 2026,
            "transition at the later state"
        );
    }

    /// Objective: Verify `aggregate_intervals` maps production-shaped facts —
    /// payload carrying only `attribution`/`content`/`negated`, exactly what the
    /// companion channels emit — onto their cognitive dimensions. A payload-key
    /// filter matched none of them, so `state_timeline` reported zero dimensions
    /// for every real conversation.
    /// Invariants: the goal/preference/emotion dimensions are produced from
    /// key-less payloads, and the two emotion states stay separate intervals.
    #[test]
    fn aggregate_intervals_maps_production_shaped_payloads() {
        let channel_fact = |id: i64, fact_type: FactType, time: i32, content: &str| Fact {
            id: Some(id),
            entity_id: 7,
            fact_type,
            time,
            payload: serde_json::json!({
                "attribution": "agent_personality",
                "content": content,
                "negated": false,
            }),
            evidence_id: None,
            created_at: i64::from(time),
            ..Fact::default()
        };
        let facts = vec![
            channel_fact(1, FactType::Emotion, 2024, "我心里很害怕"),
            channel_fact(2, FactType::Emotion, 2026, "我心里很平静"),
            channel_fact(3, FactType::Goal, 2025, "我要学 Rust"),
            channel_fact(4, FactType::Preference, 2026, "我喜欢安静"),
        ];

        let evolutions = StateEngine::new().aggregate_intervals(&facts);
        let keys: Vec<&str> = evolutions
            .iter()
            .map(|evolution| evolution.key.as_str())
            .collect();
        assert_eq!(
            keys,
            vec!["goal", "preference", "emotion"],
            "every production-shaped dimension must be reported"
        );

        let emotion = evolutions
            .iter()
            .find(|evolution| evolution.key == "emotion")
            .expect("emotion dimension present");
        assert_eq!(
            emotion.intervals.len(),
            2,
            "both emotion states are preserved as separate intervals"
        );
    }

    /// Objective: Verify `format_markdown` reports the two new dimensions.
    /// Invariants: the health summary includes relationship and identity lines.
    #[test]
    fn markdown_reports_relationship_and_identity_counts() {
        let snapshot = build_snapshot(
            7,
            "Alice".to_string(),
            "User".to_string(),
            vec![
                fact(
                    FactType::Relationship,
                    2026,
                    serde_json::json!({"target": "Bob", "kind": "friend"}),
                ),
                fact(
                    FactType::Identity,
                    2026,
                    serde_json::json!({"attribute": "occupation", "value": "engineer"}),
                ),
            ],
            &StateEngine::new(),
        );
        let md = snapshot.format_markdown();
        assert!(
            md.contains("Relationships: 1"),
            "markdown reports one relationship, got:\n{md}"
        );
        assert!(
            md.contains("Identity attributes: 1"),
            "markdown reports one identity attribute, got:\n{md}"
        );
    }

    /// Objective: Verify `FactStatus` round-trips through its stable string and
    /// falls back to Active for unknown/missing values.
    /// Invariants: as_str/from_str are inverse for all three states; any other
    /// stored string (legacy data, typos) decodes to Active, never panics.
    #[test]
    fn fact_status_roundtrips_and_falls_back_to_active() {
        for status in [
            FactStatus::Active,
            FactStatus::Superseded,
            FactStatus::Contradicted,
        ] {
            assert_eq!(
                FactStatus::parse(status.as_str()),
                status,
                "as_str/parse must be inverse for {status:?}"
            );
        }
        for unknown in ["", "expired", "archived", "PENDING", "superseded "] {
            assert_eq!(
                FactStatus::parse(unknown),
                FactStatus::Active,
                "unknown status `{unknown}` must fall back to Active"
            );
        }
    }

    /// Objective: Verify deserializing a legacy Fact JSON (without the v0.3
    /// fields) fills defaults instead of failing — old persisted snapshots must
    /// load cleanly.
    /// Invariants: missing confidence → 1.0; missing derived_from → empty;
    /// missing status → Active.
    #[test]
    fn legacy_fact_json_deserializes_with_defaults() {
        let legacy = r#"{"id":1,"entity_id":7,"fact_type":"Preference","time":2026,
                        "payload":{"content":"likes Rust"},"evidence_id":null,"created_at":1}"#;
        let fact: Fact = serde_json::from_str(legacy).expect("legacy Fact JSON must deserialize");
        assert_eq!(fact.confidence, 1.0, "missing confidence defaults to 1.0");
        assert!(
            fact.derived_from.is_empty(),
            "missing derived_from defaults to empty derivation chain"
        );
        assert_eq!(
            fact.status,
            FactStatus::Active,
            "missing status defaults to Active"
        );
    }

    /// Objective: Verify the derived_from chain is preserved through JSON and
    /// that FactStatus serializes to its stable snake_case string.
    /// Invariants: a fact with status/derived_from round-trips losslessly.
    #[test]
    fn fact_provenance_fields_roundtrip_json() {
        let original = Fact {
            id: Some(9),
            entity_id: 7,
            fact_type: FactType::Preference,
            time: 2026,
            payload: serde_json::json!({"content": "prefers Rust"}),
            evidence_id: Some(3),
            created_at: 2026,
            confidence: 0.85,
            derived_from: vec![4, 5],
            status: FactStatus::Superseded,
        };
        let json = serde_json::to_string(&original).expect("serialize Fact");
        let decoded: Fact = serde_json::from_str(&json).expect("deserialize Fact");
        assert_eq!(decoded.id, original.id, "id round-trips");
        assert_eq!(decoded.confidence, 0.85, "confidence round-trips");
        assert_eq!(decoded.derived_from, vec![4, 5], "derived_from round-trips");
        assert_eq!(
            decoded.status,
            FactStatus::Superseded,
            "status round-trips as snake_case"
        );
        assert!(
            json.contains("\"status\":\"superseded\""),
            "status serializes to stable snake_case string, got {json}"
        );
    }

    /// Objective: Verify `Fact::default()` provides safe v0.3 field defaults so
    /// every existing construction site can use `..Fact::default()`.
    /// Invariants: default is Active, confidence 1.0, empty derivation chain.
    #[test]
    fn fact_default_has_safe_v03_fields() {
        let fact = Fact::default();
        assert_eq!(fact.status, FactStatus::Active, "default status is Active");
        assert_eq!(fact.confidence, 1.0, "default confidence is 1.0");
        assert!(
            fact.derived_from.is_empty(),
            "default derivation chain is empty"
        );
        assert_eq!(fact.id, None, "default fact has no id");
    }
}
