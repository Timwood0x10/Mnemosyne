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

/// Typed current state (replace serde_json::Value eventually).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntityState {
    pub goals: Vec<Fact>,
    pub preferences: Vec<Fact>,
    pub emotion_trend: Vec<Fact>,
    pub recent_events: Vec<Fact>,
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
    pub fn aggregate(&self, facts: &[Fact]) -> EntityState {
        if self.aggregators.is_empty() {
            return EntityState::default();
        }
        let mut state = EntityState::default();
        for agg in &self.aggregators {
            let _value = agg.aggregate(facts);
            // Dispatch by aggregator type — this will be typed later
            // For V1 we collect all facts into the state
            state.recent_events.extend(
                facts
                    .iter()
                    .filter(|f| f.fact_type == FactType::Event)
                    .cloned(),
            );
            state.preferences.extend(
                facts
                    .iter()
                    .filter(|f| f.fact_type == FactType::Preference)
                    .cloned(),
            );
            state.goals.extend(
                facts
                    .iter()
                    .filter(|f| f.fact_type == FactType::Goal)
                    .cloned(),
            );
            state.emotion_trend.extend(
                facts
                    .iter()
                    .filter(|f| f.fact_type == FactType::Emotion)
                    .cloned(),
            );
        }
        state
    }
}

impl Default for StateEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// FactStore trait (persistence)
// ═══════════════════════════════════════════════════════════════════════════

/// Storage for Facts — the only write path in the Knowledge Store.
///
/// V1 implementation wraps SQLite with batch inserts (reusing EvidenceWriter's
/// transaction pattern). Future implementations can switch to LanceDB.
pub trait FactStore: Send + Sync {
    fn insert_fact(&self, fact: &Fact) -> i64;
    fn insert_batch(&self, facts: &[Fact]) -> usize;
    fn get_facts(&self, entity_id: i64) -> Vec<Fact>;
    fn get_facts_by_type(&self, entity_id: i64, fact_type: FactType) -> Vec<Fact>;
    fn get_timeline(&self, entity_id: i64) -> Vec<Fact>;
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
    pub fn format_markdown(&self) -> String {
        let mut out = format!("# {}\n\n", self.entity_name);
        out.push_str(&format!("- Type: {}\n\n", self.entity_type));
        out.push_str("## Timeline\n\n");
        for fact in &self.timeline {
            let ft = format!("{:?}", fact.fact_type);
            out.push_str(&format!("- {} [{}] {}\n", fact.time, ft, fact.payload));
        }
        out
    }

    pub fn format_json(&self) -> serde_json::Value {
        serde_json::json!(self)
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
