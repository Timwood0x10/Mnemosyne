//! Cognitive state: aggregators, the derived `EntityState` view and the
//! `StateEngine` that rebuilds current state plus state history from facts.

use serde::{Deserialize, Serialize};

use super::{Fact, FactType};

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
    /// The most recent fact for each relationship target (long-term ties).
    pub relationships: Vec<Fact>,
    /// The most recent fact for each identity attribute (who the entity is).
    pub identity_attributes: Vec<Fact>,
    /// Values emitted by application-specific aggregators.
    pub extensions: Vec<serde_json::Value>,
    /// Optional historical view: per-dimension state intervals with
    /// deterministic transitions. Deliberately a generic `Vec<StateEvolution>`
    /// — NOT a per-dimension struct (that would turn `StateEngine` into a god
    /// object). Populated by [`StateEngine::aggregate_intervals`].
    pub state_intervals: Vec<crate::state::StateEvolution>,
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
        // The latest fact per relationship target, plus per identity
        // attribute — completing the five-dimension cognitive model
        // (identity, preference, goal, emotion, relationship).
        let relationships = latest_by_payload_key(
            &chronological,
            FactType::Relationship,
            &["target", "with", "object", "content"],
        );
        let identity_attributes = latest_by_payload_key(
            &chronological,
            FactType::Identity,
            &["attribute", "identity", "key", "content"],
        );
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
            relationships,
            identity_attributes,
            extensions,
            state_intervals: Vec::new(),
        }
    }

    /// Build the state *history* for an entity: per-dimension validity
    /// intervals plus deterministic transitions between them.
    ///
    /// This is the companion to [`StateEngine::aggregate`]:
    ///
    /// - `aggregate()` answers "What is the current state?"
    /// - `aggregate_intervals()` answers "How did the current state emerge?"
    ///
    /// Like `aggregate`, this is a pure, deterministic, stateless projection:
    /// the same facts always produce the same intervals. Facts are never
    /// mutated or removed — the intervals are a derived view.
    #[must_use]
    pub fn aggregate_intervals(&self, facts: &[Fact]) -> Vec<crate::state::StateEvolution> {
        use crate::state::COGNITIVE_DIMENSIONS;
        let mut evolutions = Vec::new();
        for &(fact_type, value_keys) in COGNITIVE_DIMENSIONS {
            if let Some(evolution) =
                crate::state::intervals_for_dimension(facts, fact_type, value_keys)
            {
                evolutions.push(evolution);
            }
        }
        evolutions
    }
}

impl Default for StateEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Keep only the newest fact for each semantic payload key.
///
/// Facts that carry one of the given keys are bucketed by that key (newest
/// wins). Facts WITHOUT any key are distinct facts (e.g. observation-compiler
/// output has `{action, subject, object}` and no semantic key): they must not
/// all fold into a single "default" bucket — that silently dropped all but
/// the last. They are bucketed by their full payload instead, so different
/// facts each survive while exact duplicates still dedupe to the newest.
fn latest_by_payload_key(facts: &[Fact], fact_type: FactType, keys: &[&str]) -> Vec<Fact> {
    let mut latest = std::collections::BTreeMap::new();
    for fact in facts.iter().filter(|fact| fact.fact_type == fact_type) {
        let semantic_key = keys
            .iter()
            .find_map(|key| fact.payload.get(*key).and_then(|value| value.as_str()))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                serde_json::to_string(&fact.payload).unwrap_or_else(|_| "{}".to_string())
            });
        latest.insert(semantic_key, fact.clone());
    }
    latest.into_values().collect()
}
