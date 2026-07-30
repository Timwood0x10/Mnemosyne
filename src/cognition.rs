//! Cognition Engine — core types for the unified cognitive compiler.
//!
//! This module defines the cross-cutting types that replace ad-hoc structures
//! from LoreScope and Memory Distillation with a unified data model.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// A single atomic fact. Immutable once written.
/// State is derived from facts via aggregation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub id: Option<i64>,
    pub entity_id: i64,
    pub fact_type: FactType,
    pub time: i32,
    pub payload: serde_json::Value,
    pub evidence_id: Option<i64>,
    pub created_at: i64,
}

// ═══════════════════════════════════════════════════════════════════════════
// Rule trait (Observation → Fact)
// ═══════════════════════════════════════════════════════════════════════════

/// Transforms an Observation into zero or more Facts.
pub trait Rule: Send + Sync {
    fn apply(&self, observation: &Observation) -> Vec<Fact>;
}

// ═══════════════════════════════════════════════════════════════════════════
// State Aggregator trait (Fact → State)
// ═══════════════════════════════════════════════════════════════════════════

/// Aggregates Facts into a typed state value.
/// Deterministic and stateless — same facts → same state.
pub trait StateAggregator: Send + Sync {
    fn aggregate(&self, facts: &[Fact]) -> serde_json::Value;
}

/// Aggregates Emotion facts into trend state.
///
/// Scans Emotion-type facts for `trend` and `dimension` fields,
/// then summarizes into Rising / Stable / Falling for each dimension.
pub struct EmotionAggregator;

impl StateAggregator for EmotionAggregator {
    fn aggregate(&self, facts: &[Fact]) -> serde_json::Value {
        let mut result = std::collections::HashMap::new();
        for fact in facts.iter().filter(|f| f.fact_type == FactType::Emotion) {
            let dimension = fact
                .payload
                .get("dimension")
                .and_then(|v| v.as_str())
                .unwrap_or("general")
                .to_string();
            let trend = fact
                .payload
                .get("trend")
                .and_then(|v| v.as_str())
                .unwrap_or("stable")
                .to_string();
            result.insert(dimension, trend);
        }
        if result.is_empty() {
            result.insert("general".to_string(), "stable".to_string());
        }
        serde_json::json!(result)
    }
}

/// Current state rebuilt deterministically from immutable facts.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntityState {
    /// The most recent fact for each distinct goal.
    pub goals: Vec<Fact>,
    /// The most recent fact for each preference topic.
    pub preferences: Vec<Fact>,
    /// Emotion facts in chronological order for trend reconstruction.
    pub emotion_trend: Vec<Fact>,
    /// The most recent events, newest first.
    pub recent_events: Vec<Fact>,
    /// Values emitted by application-specific aggregators.
    pub extensions: Vec<serde_json::Value>,
}

// ═══════════════════════════════════════════════════════════════════════════
// State Engine (orchestrates aggregators)
// ═══════════════════════════════════════════════════════════════════════════

/// The State Engine takes Facts and produces Current State.
/// It does NOT create new facts — only aggregates existing ones.
pub struct StateEngine {
    aggregators: Vec<Box<dyn StateAggregator>>,
}

impl StateEngine {
    pub fn new() -> Self {
        StateEngine {
            aggregators: Vec::new(),
        }
    }

    pub fn add_aggregator(&mut self, agg: Box<dyn StateAggregator>) {
        self.aggregators.push(agg);
    }

    /// Aggregate all facts for an entity into a structured state.
    ///
    /// Built-in state is always available. Optional aggregators only extend the
    /// result and therefore cannot accidentally make the core state empty.
    pub fn aggregate(&self, facts: &[Fact]) -> EntityState {
        let mut chronological = facts.to_vec();
        chronological.sort_by_key(|fact| (fact.time, fact.created_at, fact.id.unwrap_or(0)));

        let goals = latest_by_payload_key(&chronological, FactType::Goal, &["goal", "content"]);
        let preferences = latest_by_payload_key(
            &chronological,
            FactType::Preference,
            &["topic", "preference", "content"],
        );
        let emotion_trend = chronological
            .iter()
            .filter(|fact| fact.fact_type == FactType::Emotion)
            .cloned()
            .collect();
        let recent_events = chronological
            .iter()
            .rev()
            .filter(|fact| fact.fact_type == FactType::Event)
            .take(20)
            .cloned()
            .collect();
        let extensions = self
            .aggregators
            .iter()
            .map(|aggregator| aggregator.aggregate(facts))
            .collect();

        EntityState {
            goals,
            preferences,
            emotion_trend,
            recent_events,
            extensions,
        }
    }
}

impl Default for StateEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Keep only the newest fact for each semantic payload key.
fn latest_by_payload_key(facts: &[Fact], fact_type: FactType, keys: &[&str]) -> Vec<Fact> {
    let mut latest = std::collections::BTreeMap::new();
    for fact in facts.iter().filter(|fact| fact.fact_type == fact_type) {
        let semantic_key = keys
            .iter()
            .find_map(|key| fact.payload.get(*key).and_then(|value| value.as_str()))
            .unwrap_or("default")
            .to_owned();
        latest.insert(semantic_key, fact.clone());
    }
    latest.into_values().collect()
}

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
}

// ═══════════════════════════════════════════════════════════════════════════
// EntitySnapshot (query-time view, never stored)
// ═══════════════════════════════════════════════════════════════════════════

/// A point-in-time view of an entity, computed from facts at query time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySnapshot {
    pub entity_id: i64,
    pub entity_name: String,
    pub entity_type: String,
    pub state: EntityState,
    pub timeline: Vec<Fact>,
}

impl EntitySnapshot {
    /// Format the snapshot for human inspection.
    pub fn format_markdown(&self) -> String {
        let mut out = format!("# {}\n\n", self.entity_name);
        out.push_str(&format!("- Type: {}\n\n", self.entity_type));
        out.push_str("## Current State\n\n");
        out.push_str(&format!("- Goals: {}\n", self.state.goals.len()));
        out.push_str(&format!(
            "- Preferences: {}\n",
            self.state.preferences.len()
        ));
        out.push_str(&format!(
            "- Recent events: {}\n\n",
            self.state.recent_events.len()
        ));
        out.push_str("## Timeline\n\n");
        for fact in &self.timeline {
            let fact_type = format!("{:?}", fact.fact_type);
            out.push_str(&format!(
                "- {} [{}] {}\n",
                fact.time, fact_type, fact.payload
            ));
        }
        out
    }

    /// Format the complete structured snapshot.
    pub fn format_json(&self) -> serde_json::Value {
        serde_json::json!(self)
    }

    /// Format stable, compact text for an embedding provider.
    pub fn format_embedding(&self) -> String {
        let mut parts = vec![
            format!("Name: {}", self.entity_name),
            format!("Type: {}", self.entity_type),
        ];
        parts.extend(
            self.timeline
                .iter()
                .take(20)
                .map(|fact| format!("{:?}: {}", fact.fact_type, fact.payload)),
        );
        parts.join("\n")
    }

    /// Format a bounded prompt fragment for an AI consumer.
    pub fn format_prompt(&self) -> String {
        format!(
            "Use the following evidence-backed entity state. Do not invent facts not present here.\n\n{}",
            self.format_markdown()
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// CognitiveContext (final output to AI)
// ═══════════════════════════════════════════════════════════════════════════

/// The structured context delivered to the AI agent.
/// Replaces RAG-style text retrieval entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CognitiveContext {
    pub primary_entity: Option<EntitySnapshot>,
    pub related_entities: Vec<EntitySnapshot>,
    pub facts: Vec<Fact>,
    pub timeline_window: (Option<i32>, Option<i32>),
}

// ═══════════════════════════════════════════════════════════════════════════
// SnapshotBuilder — constructs EntitySnapshot from Facts
// ═══════════════════════════════════════════════════════════════════════════

/// Builds an [`EntitySnapshot`] from facts and a [`StateEngine`].
///
/// Snapshots are never stored — always computed at query time.
pub fn build_snapshot(
    entity_id: i64,
    entity_name: String,
    entity_type: String,
    facts: Vec<Fact>,
    state_engine: &StateEngine,
) -> EntitySnapshot {
    // Timeline is just facts sorted by time
    let mut timeline = facts.clone();
    timeline.sort_by_key(|f| f.time);
    timeline.reverse(); // most recent first

    // Aggregate state from all facts
    let state = state_engine.aggregate(&facts);

    EntitySnapshot {
        entity_id,
        entity_name,
        entity_type,
        state,
        timeline,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ContextBuilder — constructs CognitiveContext from queries
// ═══════════════════════════════════════════════════════════════════════════

/// Builds a [`CognitiveContext`] ready for AI consumption.
///
/// This is the final output of the cognitive query engine — replaces
/// RAG-style text retrieval entirely.
pub fn build_context(
    primary: Option<EntitySnapshot>,
    related: Vec<EntitySnapshot>,
    all_facts: Vec<Fact>,
    window: (Option<i32>, Option<i32>),
) -> CognitiveContext {
    CognitiveContext {
        primary_entity: primary,
        related_entities: related,
        facts: all_facts,
        timeline_window: window,
    }
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
}
