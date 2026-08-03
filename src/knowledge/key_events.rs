//! Key-event extraction from a person's full trajectory.
//!
//! Per the external-knowledge plan §key-events: trajectory nodes are **key
//! events** (not chapters — chapters are a corpus artifact; real memory has
//! "something happened at time T"). The tool captures the complete trajectory
//! (`participated_in` edges + original-text evidence) and surfaces the most
//! important events with transparent scores and evidence anchors. **The tool
//! supplies evidence; the AI consuming the tool performs personality/turning
//! analysis** — this module never emits personality conclusions.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{EvidenceSourceType, KnowledgeEdge};

/// Minimum combined importance for an event to be surfaced as "key".
pub const KEY_EVENT_THRESHOLD: f64 = 0.7;

/// Maximum evidence snippets attached per key event (bounded output).
pub const MAX_EVIDENCE_PER_EVENT: usize = 5;

/// Evidence-count → [0,1] score. More corroborating original text = stronger.
#[must_use]
pub fn evidence_score(count: usize) -> f64 {
    match count {
        0 => 0.0,
        1 => 0.3,
        2..=4 => 0.5,
        5..=9 => 0.8,
        _ => 1.0,
    }
}

/// Participant-count → [0,1] score. More involved entities = higher network
/// centrality (a turning event typically involves many characters).
#[must_use]
pub fn participants_score(count: usize) -> f64 {
    match count {
        0 => 0.0,
        1..=2 => 0.3,
        3..=4 => 0.6,
        5..=7 => 0.8,
        _ => 1.0,
    }
}

/// Combine the three signals into a transparent [0,1] importance score.
///
/// - 50% evidence richness, 30% network centrality, 20% turning-point bonus.
#[must_use]
pub fn importance_score(evidence_count: usize, participant_count: usize, turning: bool) -> f64 {
    let base = 0.5 * evidence_score(evidence_count) + 0.3 * participants_score(participant_count);
    if turning { (base + 0.2).min(1.0) } else { base }
}

/// One surfaced key event with its transparent score and evidence anchors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyEvent {
    /// Event timestamp (chapter number in corpus-backed data; wall time for
    /// real memories). Used for ordering, not as a corpus concept.
    pub time: i32,
    /// Event title (the event object's name, e.g. "第53回 …").
    pub title: String,
    /// Participants in the event.
    pub participants: Vec<String>,
    /// Combined importance in [0,1] (evidence + centrality + turning).
    pub importance_score: f64,
    /// Whether this event was flagged as a turning point (isolated in time).
    pub turning_point: bool,
    /// Original-text evidence snippets (bounded by [`MAX_EVIDENCE_PER_EVENT`]).
    pub evidence: Vec<String>,
}

/// The tool's output: a person's trajectory distilled to key events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyEventsResult {
    /// Canonical entity name queried.
    pub entity: String,
    /// Source/document title the events were scoped to.
    pub source: String,
    /// Total `participated_in` events on the full trajectory.
    pub total_events: usize,
    /// Key events above [`KEY_EVENT_THRESHOLD`], ordered by time.
    pub key_events: Vec<KeyEvent>,
}

/// Extract key events for a person from the knowledge store.
///
/// # Errors
///
/// - [`Error::Storage`] on store query failure.
/// - [`Error::NotFound`]-style message when the entity is unknown (returns an
///   empty result with `total_events = 0` — an absent person is not fatal).
pub async fn extract_key_events(
    store: &dyn KnowledgeStore,
    name: &str,
    doc_title: Option<&str>,
    threshold: f64,
) -> Result<KeyEventsResult> {
    let doc_id = match doc_title {
        Some(title) => match store.find_document_by_title(title).await? {
            Some(doc) => Some(doc.id),
            None => None,
        },
        None => None,
    };
    // Resolve through the alias-aware lookup so a corpus-discovered given name
    // ("流苏") is reachable by the full name ("白流苏") and vice versa.
    let Some(object) = store.find_object_by_alias(name, doc_id).await? else {
        return Ok(KeyEventsResult {
            entity: name.to_string(),
            source: doc_title.unwrap_or("").to_string(),
            total_events: 0,
            key_events: Vec::new(),
        });
    };

    // Full trajectory: every edge touching this entity, time-ordered.
    let edges = store.get_edges_touching(object.id).await?;
    let participated: Vec<&KnowledgeEdge> = edges
        .iter()
        .filter(|e| e.predicate == "participated_in")
        .collect();
    let total_events = participated.len();

    // Participants per event = the distinct entities touching the EVENT
    // OBJECT (edge.target_id). Counting only the participated_in edge's two
    // endpoints would always yield 1 (person → event pair), starving the
    // centrality signal; the event object's neighbours reflect the event's
    // true network centrality (e.g. 聚义厅 events touch dozens of people).
    let mut participants_by_edge: Vec<Vec<String>> = Vec::with_capacity(participated.len());
    for edge in &participated {
        let mut names = Vec::new();
        let touching = store.get_edges_touching(edge.target_id).await?;
        let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
        for e in &touching {
            for endpoint in [e.source_id, e.target_id] {
                if endpoint == edge.target_id || !seen.insert(endpoint) {
                    continue;
                }
                if let Some(obj) = store.get_object(endpoint).await? {
                    if obj.name != name {
                        names.push(obj.name);
                    }
                }
            }
        }
        participants_by_edge.push(names);
    }

    // Evidence volume per event. Evidence is anchored to the EVENT OBJECT
    // (edge.target_id, source_type=object), not to the participated_in edge —
    // a single edge carries at most one snippet while the event object carries
    // the corroborating original text. Reading from the edge would collapse
    // every event to evidence_score(1)=0.3 and starve the key-event filter.
    let mut evidence_by_edge: Vec<Vec<String>> = Vec::with_capacity(participated.len());
    for edge in &participated {
        let evs = store
            .get_evidence_for(EvidenceSourceType::Object, edge.target_id)
            .await?;
        evidence_by_edge.push(
            evs.iter()
                .take(MAX_EVIDENCE_PER_EVENT)
                .map(|e| e.content.clone())
                .collect(),
        );
    }

    // Turning-point detection: an event is a turning point when its time is
    // isolated — both gaps to neighbours exceed the entity's mean gap × 1.5.
    let times: Vec<i32> = participated
        .iter()
        .map(|e| e.valid_from.unwrap_or(0))
        .collect();
    let turning: Vec<bool> = (0..times.len())
        .map(|i| is_turning_point(&times, i))
        .collect();

    let mut key_events = Vec::new();
    for (i, edge) in participated.iter().enumerate() {
        let score = importance_score(
            evidence_by_edge[i].len(),
            participants_by_edge[i].len(),
            turning[i],
        );
        if score < threshold {
            continue;
        }
        let title = match store.get_object(edge.target_id).await? {
            Some(obj) => obj.name.clone(),
            None => format!("#{}", edge.target_id),
        };
        key_events.push(KeyEvent {
            time: edge.valid_from.unwrap_or(0),
            title,
            participants: participants_by_edge[i].clone(),
            importance_score: score,
            turning_point: turning[i],
            evidence: evidence_by_edge[i].clone(),
        });
    }

    // Deterministic order by time (ties keep insertion order).
    key_events.sort_by_key(|e| e.time);

    Ok(KeyEventsResult {
        entity: name.to_string(),
        source: doc_title.unwrap_or("").to_string(),
        total_events,
        key_events,
    })
}

/// Whether the event at `index` sits on a time discontinuity.
///
/// A turning point is a jump boundary: either the gap to the previous event
/// OR the gap to the next event exceeds `mean_gap × 1.5`. An isolated event
/// always has a small gap on one side (it sits between dense clusters), so
/// requiring BOTH gaps to be large would never fire — the discontinuity is
/// the signal, on whichever side it occurs. Single-event or edge timelines
/// are never turning points (no before/after to compare).
#[must_use]
pub fn is_turning_point(times: &[i32], index: usize) -> bool {
    if times.len() < 3 || index == 0 || index + 1 >= times.len() {
        return false;
    }
    let gaps: Vec<i64> = times.windows(2).map(|w| (w[1] - w[0]) as i64).collect();
    if gaps.is_empty() {
        return false;
    }
    let mean_gap = gaps.iter().sum::<i64>() as f64 / gaps.len() as f64;
    let prev_gap = (times[index] - times[index - 1]) as i64;
    let next_gap = (times[index + 1] - times[index]) as i64;
    let threshold = mean_gap * 1.5;
    prev_gap as f64 > threshold || next_gap as f64 > threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify evidence richness is monotonic and anchored at 0.
    /// Invariants: 0 evidence → 0.0; more evidence never lowers the score.
    #[test]
    fn evidence_score_monotonic() {
        assert_eq!(evidence_score(0), 0.0, "no evidence scores zero");
        assert!(
            evidence_score(10) > evidence_score(5),
            "more evidence scores higher"
        );
        assert!(
            evidence_score(5) > evidence_score(2),
            "mid evidence scores higher"
        );
        assert!(
            evidence_score(10) > 0.0,
            "positive evidence never scores zero"
        );
        assert!(
            evidence_score(2) > 0.0,
            "two evidence snippets score above zero"
        );
    }

    /// Objective: Verify participant centrality is monotonic.
    /// Invariants: 0 participants → 0.0; more participants never lowers score.
    #[test]
    fn participants_score_monotonic() {
        assert_eq!(participants_score(0), 0.0, "no participants scores zero");
        assert!(participants_score(8) > participants_score(3));
        assert!(participants_score(3) > participants_score(1));
    }

    /// Objective: Verify the combined score stays in [0,1] and rewards rich
    /// signals.
    /// Invariants: high evidence + high participants > low + low; never > 1.0.
    #[test]
    fn importance_combines_signals() {
        let rich = importance_score(10, 8, false);
        let poor = importance_score(0, 0, false);
        assert!(rich > poor, "rich event must outscore a poor one");
        assert!((0.0..=1.0).contains(&rich));
        assert_eq!(poor, 0.0, "all-zero signals score exactly zero");
    }

    /// Objective: Verify the turning-point bonus boosts isolated events.
    /// Invariants: same signals with turning=true score higher; capped at 1.0.
    #[test]
    fn turning_point_boosts_score() {
        let base = importance_score(5, 3, false);
        let boosted = importance_score(5, 3, true);
        assert!(boosted > base, "turning flag must boost the score");
        assert!(boosted <= 1.0, "score must stay capped at 1.0");
    }

    /// Objective: Verify turning-point detection on a sparse timeline.
    /// Invariants: an isolated middle event is flagged; first/last never are.
    #[test]
    fn is_turning_point_flags_isolated_events() {
        // Times: gaps 1,1,20,1,1 → index 3 (time 23) is isolated.
        let times = vec![1, 2, 3, 23, 24, 25];
        assert!(
            is_turning_point(&times, 3),
            "event at time 23 must be a turning point"
        );
        assert!(
            !is_turning_point(&times, 0),
            "first event is never a turning point"
        );
        assert!(
            !is_turning_point(&times, 5),
            "last event is never a turning point"
        );
        assert!(
            !is_turning_point(&times, 1),
            "dense event is not a turning point"
        );
    }

    /// Objective: Verify short timelines never flag turning points.
    /// Invariants: <3 events or edge indices → false (no crash).
    #[test]
    fn short_timelines_have_no_turning_points() {
        assert!(!is_turning_point(&[], 0));
        assert!(!is_turning_point(&[1, 5], 0));
        assert!(!is_turning_point(&[1, 5, 9], 0));
        assert!(!is_turning_point(&[1, 5, 9], 2));
    }
}
