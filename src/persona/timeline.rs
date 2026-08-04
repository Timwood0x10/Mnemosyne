//! Persona evolution timeline — the "记录一个人的完整变化过程" layer (阶段C-2, C6).
//!
//! A companion AI should be able to re-derive *who a person has become* over
//! time, the way a reader reconstructs a character's life from a novel: from
//! the earliest identity, through the turning points, to the present. This
//! module rebuilds that continuous trajectory from the **accumulated** facts
//! in the fact store — like parsing 三国演义 to trace how a person went from
//! their origin to their current state.
//!
//! It follows the **mem0 v3 ADD-only accumulation policy**: facts are never
//! deleted or overwritten. A stance flip ("喜欢" → "不喜欢") keeps *both*
//! facts and is flagged as a [`MilestoneType::StanceFlip`] turning point, so
//! the full before → after arc is reconstructible at any time.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::cognition::{Fact, FactStore, FactType};
use crate::error::Result;
use crate::persona::check::is_persona_fact;

/// A time gap (in `fact.time` units) large enough to count as a turning point.
///
/// `fact.time` is a logical timestamp; the value is deliberately large so it
/// only fires on genuinely separated events.
const LARGE_GAP_THRESHOLD: i32 = 1_000_000;

/// The kind of a persona-evolution turning point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneType {
    /// A same-type fact with the opposite `negated` flag (e.g. "喜欢" → "不喜欢").
    StanceFlip,
    /// The first fact of a never-before-seen `fact_type`.
    NewTheme,
    /// A large `fact.time` gap between two consecutive facts.
    LargeGap,
}

/// A single turning point in a person's evolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    /// The fact that triggered the turning point.
    pub fact: Fact,
    /// The kind of turning point.
    pub milestone_type: MilestoneType,
    /// A human-readable note describing the change.
    pub note: String,
}

/// A full reconstruction of one entity's persona evolution.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EvolutionTimeline {
    /// The earliest Identity / persona fact (the origin).
    pub start: Option<Fact>,
    /// Key turning points in chronological order.
    pub milestones: Vec<Milestone>,
    /// The latest persona fact (the present).
    pub current: Option<Fact>,
    /// The complete, time-ordered fact list (ADD-only, nothing dropped).
    pub trajectory: Vec<Fact>,
}

/// Build an [`EvolutionTimeline`] from an entity's accumulated facts.
///
/// The facts are sorted by time and organized into `起点 → 关键转变点 → 现状`,
/// with the full trajectory preserved for audit. Facts are never mutated,
/// removed, or collapsed — this is the ADD-only reconstruction.
#[must_use]
pub fn build_evolution_timeline(facts: &[Fact]) -> EvolutionTimeline {
    let mut chronological = facts.to_vec();
    chronological.sort_by_key(|f| (f.time, f.created_at, f.id.unwrap_or(0)));

    EvolutionTimeline {
        start: find_start(&chronological),
        milestones: find_milestones(&chronological),
        current: find_current(&chronological),
        trajectory: chronological,
    }
}

/// Read an entity's facts from the store and rebuild its evolution timeline.
///
/// # Errors
///
/// Returns a storage error when the entity's facts cannot be read.
pub fn build_timeline_for_entity(
    store: &dyn FactStore,
    entity_id: i64,
) -> Result<EvolutionTimeline> {
    let facts = store.get_facts(entity_id)?;
    Ok(build_evolution_timeline(&facts))
}

/// The origin: the earliest Identity fact, else the earliest persona fact,
/// else the overall earliest fact.
fn find_start(facts: &[Fact]) -> Option<Fact> {
    facts
        .iter()
        .find(|f| f.fact_type == FactType::Identity)
        .cloned()
        .or_else(|| facts.iter().find(|f| is_persona_fact(f)).cloned())
        .or_else(|| facts.first().cloned())
}

/// The present: the latest persona fact, else the overall latest fact.
fn find_current(facts: &[Fact]) -> Option<Fact> {
    facts
        .iter()
        .rev()
        .find(|f| is_persona_fact(f))
        .cloned()
        .or_else(|| facts.last().cloned())
}

/// Detect turning points across the time-ordered facts.
///
/// - **StanceFlip**: a `negated` fact appears after an earlier same-type fact
///   with the opposite `negated` flag. Both facts are retained.
/// - **NewTheme**: the first fact of a previously-unseen `fact_type`.
/// - **LargeGap**: a `fact.time` jump larger than [`LARGE_GAP_THRESHOLD`].
fn find_milestones(facts: &[Fact]) -> Vec<Milestone> {
    let mut milestones = Vec::new();
    let mut last_negated: HashMap<FactType, bool> = HashMap::new();
    let mut seen_types: HashSet<FactType> = HashSet::new();
    let mut prev_time: Option<i32> = None;

    for fact in facts {
        // Stance flip: same fact_type, opposite negated flag.
        if let Some(negated) = fact
            .payload
            .get("negated")
            .and_then(serde_json::Value::as_bool)
        {
            if let Some(prev) = last_negated.get(&fact.fact_type) {
                if *prev != negated {
                    milestones.push(Milestone {
                        fact: fact.clone(),
                        milestone_type: MilestoneType::StanceFlip,
                        note: format!(
                            "stance flip: {:?} went from negated={prev} to negated={negated}",
                            fact.fact_type
                        ),
                    });
                }
            }
            last_negated.insert(fact.fact_type, negated);
        }

        // New theme: first fact of a fresh fact_type.
        if seen_types.insert(fact.fact_type) {
            milestones.push(Milestone {
                fact: fact.clone(),
                milestone_type: MilestoneType::NewTheme,
                note: format!("new theme: {:?}", fact.fact_type),
            });
        }

        // Large gap between consecutive facts.
        if let Some(prev) = prev_time {
            let gap = fact.time - prev;
            if gap > LARGE_GAP_THRESHOLD {
                milestones.push(Milestone {
                    fact: fact.clone(),
                    milestone_type: MilestoneType::LargeGap,
                    note: format!("large time gap: {gap} units since the previous fact"),
                });
            }
        }
        prev_time = Some(fact.time);
    }

    milestones
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact_store::SqliteFactStore;

    fn fact(id: i64, fact_type: FactType, time: i32, negated: Option<bool>, content: &str) -> Fact {
        let mut payload = serde_json::json!({ "content": content });
        if let Some(neg) = negated {
            payload["negated"] = serde_json::Value::from(neg);
            payload["attribution"] = serde_json::Value::from("agent_personality");
        }
        Fact {
            id: Some(id),
            entity_id: 7,
            fact_type,
            time,
            payload,
            evidence_id: None,
            created_at: i64::from(time),
        }
    }

    /// Objective: Verify a stance flip (喜欢 → 不喜欢) is detected as a turning
    /// point while BOTH facts are retained (ADD-only).
    /// Invariants: one StanceFlip milestone; trajectory keeps both facts.
    #[test]
    fn stance_flip_is_detected_and_both_facts_kept() {
        let facts = vec![
            fact(1, FactType::Preference, 2024, Some(false), "我喜欢应酬"),
            fact(2, FactType::Preference, 2026, Some(true), "我不喜欢应酬"),
        ];
        let timeline = build_evolution_timeline(&facts);

        let flips: Vec<&Milestone> = timeline
            .milestones
            .iter()
            .filter(|m| m.milestone_type == MilestoneType::StanceFlip)
            .collect();
        assert_eq!(flips.len(), 1, "one stance flip milestone");
        assert_eq!(flips[0].fact.fact_type, FactType::Preference);

        assert_eq!(
            timeline.trajectory.len(),
            2,
            "ADD-only: both facts must remain in the trajectory"
        );
        assert_eq!(
            timeline.trajectory[0].payload["negated"], false,
            "the earlier fact is preserved, not overwritten"
        );
        assert_eq!(
            timeline.trajectory[1].payload["negated"], true,
            "the later fact is also preserved"
        );
    }

    /// Objective: Verify start/current selection and time ordering.
    /// Invariants: start is the earliest Identity fact; current is the latest
    /// persona fact; trajectory is ascending by time.
    #[test]
    fn start_and_current_are_selected_by_time() {
        let facts = vec![
            fact(1, FactType::Identity, 2020, None, "我是白流苏"),
            fact(2, FactType::Preference, 2024, Some(false), "我喜欢安稳"),
            fact(3, FactType::Preference, 2026, Some(true), "我不喜欢应酬"),
        ];
        let timeline = build_evolution_timeline(&facts);

        assert_eq!(
            timeline.start.as_ref().map(|f| f.time),
            Some(2020),
            "start is the earliest identity fact"
        );
        assert_eq!(
            timeline.current.as_ref().map(|f| f.time),
            Some(2026),
            "current is the latest persona fact"
        );
        let times: Vec<i32> = timeline.trajectory.iter().map(|f| f.time).collect();
        assert_eq!(times, vec![2020, 2024, 2026], "trajectory is time-ordered");
    }

    /// Objective: Verify a large time gap is flagged as a turning point.
    /// Invariants: a gap larger than LARGE_GAP_THRESHOLD produces a LargeGap
    /// milestone.
    #[test]
    fn large_time_gap_is_a_milestone() {
        let facts = vec![
            fact(1, FactType::Event, 1000, None, "event A"),
            fact(
                2,
                FactType::Event,
                1000 + LARGE_GAP_THRESHOLD + 1,
                None,
                "event B",
            ),
        ];
        let timeline = build_evolution_timeline(&facts);
        assert!(
            timeline
                .milestones
                .iter()
                .any(|m| m.milestone_type == MilestoneType::LargeGap),
            "a large time gap must be a milestone"
        );
    }

    /// Objective: Verify a new theme (first fact of a fresh fact_type) is
    /// flagged without disturbing stance-flip detection.
    /// Invariants: a second topic produces a NewTheme milestone.
    #[test]
    fn new_theme_is_a_milestone() {
        let facts = vec![
            fact(1, FactType::Preference, 2024, Some(false), "我喜欢安稳"),
            fact(2, FactType::Goal, 2026, Some(false), "我要去上海"),
        ];
        let timeline = build_evolution_timeline(&facts);
        assert!(
            timeline
                .milestones
                .iter()
                .any(|m| m.milestone_type == MilestoneType::NewTheme
                    && m.fact.fact_type == FactType::Goal),
            "a fresh fact_type must be flagged as a new theme"
        );
    }

    /// Objective: Verify the whole timeline is rebuilt from the store and that
    /// nothing is lost (ADD-only).
    /// Invariants: count matches the inserted facts; milestone detection works.
    #[test]
    fn timeline_rebuilt_from_store() {
        let store = SqliteFactStore::open_in_memory().expect("fact store");
        let entity_id = store
            .resolve_agent("tenant-a", "agent-bailiusu")
            .expect("resolve");
        for mut f in [
            fact(1, FactType::Identity, 1, None, "我是白流苏"),
            fact(2, FactType::Preference, 2, Some(false), "我喜欢应酬"),
            fact(3, FactType::Preference, 3, Some(true), "我不喜欢应酬"),
        ] {
            f.entity_id = entity_id;
            store.insert_fact(&f).expect("insert fact");
        }

        let timeline = build_timeline_for_entity(&store, entity_id).expect("build timeline");
        assert_eq!(timeline.trajectory.len(), 3, "all three facts are rebuilt");
        assert!(
            timeline
                .milestones
                .iter()
                .any(|m| m.milestone_type == MilestoneType::StanceFlip),
            "the stance flip is detected from the store"
        );
        assert_eq!(
            timeline.start.as_ref().map(|f| f.fact_type),
            Some(FactType::Identity),
            "the origin is the identity fact"
        );
    }
}
