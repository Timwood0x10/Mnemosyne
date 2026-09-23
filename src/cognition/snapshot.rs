//! Query-time views: `EntitySnapshot` and the `CognitiveContext` delivered to
//! the agent. Snapshots are never stored, only computed from facts.

use serde::{Deserialize, Serialize};

use super::Fact;
use super::state::{EntityState, StateEngine};

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
            "- Relationships: {}\n",
            self.state.relationships.len()
        ));
        out.push_str(&format!(
            "- Identity attributes: {}\n",
            self.state.identity_attributes.len()
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
