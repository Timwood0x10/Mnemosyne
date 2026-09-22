//! Cognitive State History — the "how did the current state emerge?" layer.
//!
//! v0.3 turns `StateEngine` from a latest-wins aggregator into a state
//! evolution engine. Alongside `aggregate()` ("what is the current state?"),
//! [`aggregate_intervals`] answers "what was true before, and how did it
//! change?".
//!
//! Design rules (frozen for v0.3, see `plan/cognitive-state-v03.md`):
//!
//! - A [`StateInterval`] is a *state validity* window over a semantic key
//!   (the `to` field is `None` while the interval is still current). `Fact.time`
//!   is observation time; `from` is when the state became valid. We do not
//!   model a full bi-temporal system — without a reliable `valid_from` we fall
//!   back to the observation-derived interval.
//! - A [`StateTransition`] is a *relation between two intervals*: it references
//!   them by index, never copying them.
//! - Transition detection is **deterministic** and **allowed to be uncertain**:
//!   if no definite relation can be established we emit the intervals without a
//!   transition. We never hallucinate a transition and never use an LLM.
//! - State is a derived view. Facts are never removed, and state can always be
//!   recomputed from facts.

use serde::{Deserialize, Serialize};

use crate::cognition::{Fact, FactType};
use crate::persona::check::shared_bigrams;

/// Minimum number of shared character-bigrams required for two same-type,
/// opposite-negated facts to count as a `StanceFlip` transition. Mirrors
/// `persona/timeline.rs::STANCE_FLIP_MIN_SHARED_BIGRAMS` so the cognitive
/// layer and the persona layer agree on what counts as a stance change.
const STANCE_FLIP_MIN_SHARED_BIGRAMS: usize = 2;

/// A state-validity window over one semantic key.
///
/// `from`/`to` are **state validity times**, not observation times (§2.2 of
/// the plan). `to == None` means the state still holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateInterval {
    /// When this state became valid.
    pub from: i32,
    /// When this state ceased to be valid (`None` = still current).
    pub to: Option<i32>,
    /// The state value (a representative fact's payload).
    pub value: serde_json::Value,
    /// The dimension's state value (`payload[value_key]`) used for folding:
    /// two facts describing the same state fold into one interval.
    pub state_value: serde_json::Value,
    /// Internal fold key `(state_value, negated)` — two facts fold only when
    /// both the value and the negation agree.
    pub fold_key: (serde_json::Value, bool),
    /// Facts that establish this interval.
    pub fact_ids: Vec<i64>,
    /// Evidence anchors backing the interval.
    pub evidence_ids: Vec<i64>,
}

/// The kind of a state change between two intervals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionType {
    /// A · → · → B — continuous drift (e.g. solitary → social curiosity).
    GradualChange,
    /// A definite inversion on the same topic (喜欢 X → 不喜欢 X).
    StanceFlip,
    /// A stated intent later confirmed by behavior (说想社交 → 真的参加了活动).
    BehavioralConfirmation,
}

/// A relation between two [`StateInterval`]s.
///
/// The intervals are referenced by index into a shared
/// [`StateEvolution`]`::intervals` vector — never copied into the transition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateTransition {
    /// Index into `StateEvolution::intervals` of the earlier state.
    pub from_index: usize,
    /// Index into `StateEvolution::intervals` of the later state.
    pub to_index: usize,
    /// When the change happened (state validity time).
    pub at: i32,
    /// The kind of change.
    pub transition_type: TransitionType,
    /// Evidence anchors backing the change.
    pub evidence_ids: Vec<i64>,
}

/// The complete state history for one semantic dimension.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct StateEvolution {
    /// The semantic key this evolution tracks (e.g. `topic: "programming"`).
    pub key: String,
    /// Time-ordered state intervals (ADD-only, nothing dropped).
    pub intervals: Vec<StateInterval>,
    /// Deterministic transitions between consecutive intervals. May be empty:
    /// a change without a definite relation is reported as intervals only.
    pub transitions: Vec<StateTransition>,
}

/// Build state intervals for one cognitive dimension.
///
/// The dimension is selected by [`FactType`] — the same filter
/// [`StateEngine::aggregate`](crate::cognition::StateEngine::aggregate) uses —
/// and **not** by a payload field. Every fact emitted by the production
/// compilers carries only `content`/`negated`/`attribution`, so selecting the
/// dimension by payload key silently dropped all real facts and made
/// `state_timeline` return zero dimensions for every real conversation.
///
/// `value_keys` is a priority list of payload fields naming the *state value*
/// (the thing that changes: Python → Rust). The first present field wins; when
/// none is present the entire payload becomes the fold value, so two distinct
/// key-less facts never collapse into one interval.
///
/// Groups the facts by the resolved state value and splits them into validity
/// windows: each *distinct* value in `time` order becomes an interval that runs
/// from that fact's time until the next distinct value. Consecutive facts with
/// an identical value **and** negation fold into one interval. Returns `None`
/// when there are no facts for this dimension.
#[must_use]
pub fn intervals_for_dimension(
    facts: &[Fact],
    fact_type: FactType,
    value_keys: &[&str],
) -> Option<StateEvolution> {
    let mut dimension_facts: Vec<&Fact> = facts
        .iter()
        .filter(|fact| fact.fact_type == fact_type)
        .collect();
    if dimension_facts.is_empty() {
        return None;
    }
    dimension_facts.sort_by_key(|fact| (fact.time, fact.created_at, fact.id.unwrap_or(0)));

    let mut evolution = StateEvolution {
        key: fact_type.as_str().to_string(),
        ..StateEvolution::default()
    };
    let mut current: Option<StateInterval> = None;
    for fact in dimension_facts {
        // The interval value is the fact's full payload: transition detection
        // needs `negated`/`keyword`/`content` fields, and consumers can read
        // the dimension value through `value_key`.
        let value = fact.payload.clone();
        // Fold comparison is on the dimension's state value (`value_key`)
        // PLUS the negation flag: "喜欢 Python" then "还是喜欢 Python" is the
        // SAME state (Python), but "喜欢应酬" then "不喜欢应酬" is a DIFFERENT
        // state even though both carry `preference: 应酬`.
        let state_value = resolve_state_value(&fact.payload, value_keys);
        let negated = fact
            .payload
            .get("negated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let fold_key = (state_value.clone(), negated);
        if let Some(interval) = &mut current {
            if interval.fold_key == fold_key {
                // Same state — extend the interval.
                interval.fact_ids.push(fact.id.unwrap_or(0));
                if let Some(evidence_id) = fact.evidence_id {
                    interval.evidence_ids.push(evidence_id);
                }
                continue;
            }
            // Different state — close the current interval.
            interval.to = Some(fact.time);
            let closed = current.take().expect("current interval just taken");
            evolution.intervals.push(closed);
        }
        let mut interval = StateInterval {
            from: fact.time,
            to: None,
            value,
            state_value,
            fold_key,
            fact_ids: vec![fact.id.unwrap_or(0)],
            evidence_ids: Vec::new(),
        };
        if let Some(evidence_id) = fact.evidence_id {
            interval.evidence_ids.push(evidence_id);
        }
        current = Some(interval);
    }
    if let Some(interval) = current.take() {
        evolution.intervals.push(interval);
    }

    // Deterministic transition detection between consecutive intervals.
    //
    // The endpoints must be the indices of the window being inspected, not of
    // the last two intervals: `intervals` does not grow inside this loop, so
    // `len() - 2`/`len() - 1` pinned every transition to the tail pair and
    // mislabelled the whole history once a dimension had three or more states.
    for (index, window) in evolution.intervals.windows(2).enumerate() {
        let (from, to) = (&window[0], &window[1]);
        if let Some(transition_type) = detect_transition(from, to) {
            evolution.transitions.push(StateTransition {
                from_index: index,
                to_index: index + 1,
                at: to.from,
                transition_type,
                evidence_ids: to.evidence_ids.clone(),
            });
        }
    }

    Some(evolution)
}

/// Resolve the fold value of a fact: the first present `value_keys` field, or
/// the whole payload when none is present.
///
/// Falling back to the payload (rather than `Null`) keeps key-less facts
/// distinct — with a `Null` fallback every such fact folded into a single
/// interval and all but the last state silently disappeared.
fn resolve_state_value(payload: &serde_json::Value, value_keys: &[&str]) -> serde_json::Value {
    value_keys
        .iter()
        .find_map(|key| payload.get(*key))
        .cloned()
        .unwrap_or_else(|| payload.clone())
}

/// Deterministic transition detection between two consecutive intervals.
///
/// Returns `None` when no definite relation exists — the change is then
/// reported as intervals only (allowed to be uncertain).
fn detect_transition(from: &StateInterval, to: &StateInterval) -> Option<TransitionType> {
    let from_content = content_of(&from.value);
    let to_content = content_of(&to.value);
    if from_content.is_empty() || to_content.is_empty() {
        return None;
    }

    // BehavioralConfirmation: the later state's content is about action
    // while the earlier was about intent. Detected via `negated`-insensitive
    // contrast on the same topic with a strong bigram overlap.
    if shared_bigrams(from_content, to_content) >= STANCE_FLIP_MIN_SHARED_BIGRAMS
        && (contains_action_word(to_content))
    {
        return Some(TransitionType::BehavioralConfirmation);
    }

    // StanceFlip: same topic, opposite negation.
    if let (Some(from_negated), Some(to_negated)) = (
        from.value
            .get("negated")
            .and_then(serde_json::Value::as_bool),
        to.value.get("negated").and_then(serde_json::Value::as_bool),
    ) {
        if from_negated != to_negated
            && shared_bigrams(from_content, to_content) >= STANCE_FLIP_MIN_SHARED_BIGRAMS
        {
            return Some(TransitionType::StanceFlip);
        }
    }

    // GradualChange: same value key present in both, content differs but the
    // topic overlaps at least partially. Keep it conservative: only claim a
    // gradual change when the two states share the topic's key (e.g. both
    // carry a `keyword` field) and are not an exact equal value.
    if from_content != to_content
        && from.value.get("keyword").is_some()
        && to.value.get("keyword").is_some()
    {
        return Some(TransitionType::GradualChange);
    }

    None
}

/// Read a stringable `content` out of an interval value (the fact payload).
fn content_of(value: &serde_json::Value) -> &str {
    value
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// A conservative action-word heuristic for BehavioralConfirmation: the later
/// state mentions a concrete performed activity. "开始喜欢" is a preference
/// expression, not a performed behavior, so generic words like 开始 are
/// deliberately excluded. We only claim confirmation when the two states also
/// share the same topic (≥ STANCE_FLIP_MIN_SHARED_BIGRAMS).
fn contains_action_word(content: &str) -> bool {
    ["参加", "去了", "做了", "报名", "主动"]
        .iter()
        .any(|word| content.contains(word))
}

/// The five cognitive dimensions `aggregate_intervals` reports on, aligned
/// with `EntityState`'s current-state dimensions.
///
/// Each entry is `(fact_type, value_keys)`:
///
/// - `fact_type` — a fact belongs to this dimension when it carries this type,
///   mirroring [`StateEngine::aggregate`](crate::cognition::StateEngine::aggregate)
///   so the current state and the state history never disagree about what
///   belongs to a dimension.
/// - `value_keys` — priority list of payload fields naming the state value; the
///   first present field wins and the whole payload is the fallback. `content`
///   leads every list because that is what actually changes between two states
///   ("喜欢 Python" → "开始喜欢 Rust"); the trailing names cover facts that
///   carry only their semantic field.
pub const COGNITIVE_DIMENSIONS: &[(FactType, &[&str])] = &[
    (FactType::Goal, &["content", "goal"]),
    (FactType::Preference, &["content", "preference", "topic"]),
    (FactType::Emotion, &["content", "emotion", "label"]),
    (
        FactType::Relationship,
        &["content", "target", "with", "object"],
    ),
    (
        FactType::Identity,
        &["content", "identity", "attribute", "key"],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::FactType;

    fn fact(
        id: i64,
        fact_type: FactType,
        time: i32,
        key: &str,
        value: &str,
        content: &str,
        negated: Option<bool>,
    ) -> Fact {
        let mut payload = serde_json::json!({
            key: value,
            "content": content,
        });
        if let Some(neg) = negated {
            payload["negated"] = serde_json::Value::from(neg);
        }
        Fact {
            id: Some(id),
            entity_id: 7,
            fact_type,
            time,
            payload,
            evidence_id: None,
            created_at: i64::from(time),
            ..Fact::default()
        }
    }

    /// Objective: Verify three time states are preserved as three intervals
    /// (ADD-only: nothing collapsed into a single latest-wins state).
    /// Invariants: 2024/2025/2026 each produce an interval; values are
    /// Python → Rust emerging → Rust dominant; to/from chain is contiguous.
    #[test]
    fn three_time_states_are_preserved_as_intervals() {
        let facts = vec![
            fact(
                1,
                FactType::Preference,
                2024,
                "preference",
                "Python",
                "我喜欢 Python",
                None,
            ),
            fact(
                2,
                FactType::Preference,
                2025,
                "preference",
                "Rust emerging",
                "开始喜欢 Rust",
                None,
            ),
            fact(
                3,
                FactType::Preference,
                2026,
                "preference",
                "Rust dominant",
                "主要使用 Rust",
                None,
            ),
        ];
        let evolution =
            intervals_for_dimension(&facts, FactType::Preference, &["content", "preference"])
                .expect("preference dimension has facts");
        assert_eq!(
            evolution.intervals.len(),
            3,
            "three states → three intervals"
        );
        let values: Vec<&str> = evolution
            .intervals
            .iter()
            .map(|i| i.value["content"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            values,
            vec!["我喜欢 Python", "开始喜欢 Rust", "主要使用 Rust"],
            "interval values carry the content for each state"
        );
        assert_eq!(evolution.intervals[0].from, 2024);
        assert_eq!(evolution.intervals[0].to, Some(2025));
        assert_eq!(evolution.intervals[1].to, Some(2026));
        assert_eq!(
            evolution.intervals[2].to, None,
            "latest interval stays current"
        );
    }

    /// Objective: Verify consecutive identical values fold into a single
    /// interval instead of duplicating state.
    /// Invariants: two same-value facts → one interval.
    #[test]
    fn identical_consecutive_values_fold_into_one_interval() {
        let facts = vec![
            fact(
                1,
                FactType::Preference,
                2024,
                "preference",
                "Python",
                "喜欢 Python",
                None,
            ),
            fact(
                2,
                FactType::Preference,
                2025,
                "preference",
                "Python",
                "喜欢 Python",
                None,
            ),
        ];
        let evolution =
            intervals_for_dimension(&facts, FactType::Preference, &["content", "preference"])
                .expect("preference dimension has facts");
        assert_eq!(
            evolution.intervals.len(),
            1,
            "identical values fold to one interval"
        );
        assert_eq!(
            evolution.intervals[0].fact_ids,
            vec![1, 2],
            "both facts anchor the folded interval"
        );
        assert_eq!(
            evolution.intervals[0].to, None,
            "folded interval is still current"
        );
    }

    /// Objective: Verify a stance flip (喜欢应酬 → 不喜欢应酬) produces a
    /// StanceFlip transition while preserving both intervals.
    /// Invariants: two intervals; one StanceFlip transition; from_index=0,
    /// to_index=1; at=2026.
    #[test]
    fn stance_flip_produces_transition_and_keeps_intervals() {
        let facts = vec![
            fact(
                1,
                FactType::Preference,
                2024,
                "preference",
                "喜欢应酬",
                "我喜欢应酬",
                Some(false),
            ),
            fact(
                2,
                FactType::Preference,
                2026,
                "preference",
                "不喜欢应酬",
                "我不喜欢应酬",
                Some(true),
            ),
        ];
        let evolution =
            intervals_for_dimension(&facts, FactType::Preference, &["content", "preference"])
                .expect("preference dimension has facts");
        assert_eq!(evolution.intervals.len(), 2, "both states preserved");
        assert_eq!(evolution.transitions.len(), 1, "one transition");
        assert_eq!(
            evolution.transitions[0].transition_type,
            TransitionType::StanceFlip
        );
        assert_eq!(evolution.transitions[0].from_index, 0);
        assert_eq!(evolution.transitions[0].to_index, 1);
        assert_eq!(evolution.transitions[0].at, 2026);
    }

    /// Objective: Verify unrelated same-type topics do NOT produce a
    /// transition — no shared bigrams → no stance flip (deterministic guard).
    /// Invariants: intervals present, transitions empty.
    #[test]
    fn unrelated_topics_produce_no_transition() {
        let facts = vec![
            fact(
                1,
                FactType::Preference,
                2024,
                "preference",
                "讨厌应酬",
                "我讨厌应酬",
                Some(true),
            ),
            fact(
                2,
                FactType::Preference,
                2026,
                "preference",
                "喜欢安稳",
                "我喜欢安稳",
                Some(false),
            ),
        ];
        let evolution =
            intervals_for_dimension(&facts, FactType::Preference, &["content", "preference"])
                .expect("preference dimension has facts");
        assert_eq!(evolution.intervals.len(), 2, "intervals preserved");
        assert!(
            evolution.transitions.is_empty(),
            "unrelated topics must not fabricate a transition"
        );
    }

    /// Objective: Verify an empty dimension yields no evolution.
    /// Invariants: None, not an empty Some.
    #[test]
    fn empty_dimension_yields_none() {
        let evolution = intervals_for_dimension(&[], FactType::Preference, &["content"]);
        assert!(evolution.is_none(), "no facts → no evolution");
    }

    /// Objective: Verify the dimension key is honored — a fact without the
    /// semantic key is excluded from that dimension.
    /// Invariants: only the fact carrying `preference` is grouped.
    #[test]
    fn dimension_key_filters_facts() {
        let facts = vec![
            fact(
                1,
                FactType::Preference,
                2024,
                "preference",
                "Python",
                "喜欢 Python",
                None,
            ),
            fact(
                2,
                FactType::Goal,
                2025,
                "goal",
                "去上海",
                "我要去上海",
                None,
            ),
        ];
        let evolution =
            intervals_for_dimension(&facts, FactType::Preference, &["content", "preference"])
                .expect("preference dimension has facts");
        assert_eq!(evolution.intervals.len(), 1, "goal fact is filtered out");
        assert_eq!(
            evolution.key, "preference",
            "evolution tracks the requested dimension"
        );
    }

    /// Objective: Verify a gradual change (both carry a `keyword`) produces a
    /// GradualChange transition.
    /// Invariants: same keyword, different content → GradualChange.
    #[test]
    fn gradual_change_is_detected_when_topic_overlaps() {
        let mut a = fact(
            1,
            FactType::Preference,
            2024,
            "preference",
            "喜欢独处",
            "喜欢独处",
            None,
        );
        let mut b = fact(
            2,
            FactType::Preference,
            2026,
            "preference",
            "喜欢热闹",
            "喜欢热闹",
            None,
        );
        a.payload["keyword"] = serde_json::Value::from("社交");
        b.payload["keyword"] = serde_json::Value::from("社交");
        let evolution =
            intervals_for_dimension(&[a, b], FactType::Preference, &["content", "preference"])
                .expect("preference dimension has facts");
        assert_eq!(evolution.transitions.len(), 1, "one gradual change");
        assert_eq!(
            evolution.transitions[0].transition_type,
            TransitionType::GradualChange
        );
    }

    /// Objective: Verify every transition points at its OWN window once a
    /// dimension has three or more intervals. The previous implementation wrote
    /// `len() - 2`/`len() - 1`, which pinned all transitions to the tail pair;
    /// the two-interval tests never exposed it.
    /// Invariants: three states → three intervals and two transitions with
    /// `(from_index, to_index)` equal to `(0, 1)` and `(1, 2)`; each referenced
    /// pair is adjacent in time (`from.to == to.from`) and moves forward.
    #[test]
    fn every_transition_references_its_own_window() {
        let mut facts = vec![
            fact(
                1,
                FactType::Preference,
                2024,
                "preference",
                "独处",
                "喜欢独处",
                None,
            ),
            fact(
                2,
                FactType::Preference,
                2025,
                "preference",
                "社交",
                "开始想社交",
                None,
            ),
            fact(
                3,
                FactType::Preference,
                2026,
                "preference",
                "热闹",
                "喜欢热闹",
                None,
            ),
        ];
        // A shared `keyword` on both sides of every window makes the change a
        // definite GradualChange, so a transition is emitted per window.
        for fact in &mut facts {
            fact.payload["keyword"] = serde_json::Value::from("社交");
        }

        let evolution =
            intervals_for_dimension(&facts, FactType::Preference, &["content", "preference"])
                .expect("preference dimension has facts");
        assert_eq!(
            evolution.intervals.len(),
            3,
            "three states → three intervals"
        );

        let windows: Vec<(usize, usize)> = evolution
            .transitions
            .iter()
            .map(|transition| (transition.from_index, transition.to_index))
            .collect();
        assert_eq!(
            windows,
            vec![(0, 1), (1, 2)],
            "each transition must reference its own window, not the tail pair"
        );

        for transition in &evolution.transitions {
            let from = &evolution.intervals[transition.from_index];
            let to = &evolution.intervals[transition.to_index];
            assert_eq!(
                from.to,
                Some(to.from),
                "transition endpoints must be adjacent intervals"
            );
            assert!(
                from.from < to.from,
                "a transition must move forward in time ({} → {})",
                from.from,
                to.from
            );
        }
    }

    /// Objective: Verify production-shaped facts still map onto their cognitive
    /// dimension. Facts emitted by the companion channels carry only
    /// `attribution`/`content`/`negated`; the old payload-key filter required a
    /// field such as `emotion`, so `state_timeline` returned zero dimensions for
    /// every real conversation.
    /// Invariants: two Emotion facts yield the `emotion` dimension with two
    /// intervals (ADD-only: neither state is folded away).
    #[test]
    fn production_shaped_payloads_map_to_their_dimension() {
        let emotion = |id: i64, time: i32, content: &str| Fact {
            id: Some(id),
            entity_id: 7,
            fact_type: FactType::Emotion,
            time,
            payload: serde_json::json!({
                "attribution": "agent_personality",
                "content": content,
                "negated": false,
            }),
            created_at: i64::from(time),
            ..Fact::default()
        };
        let facts = vec![
            emotion(1, 2024, "我心里很害怕"),
            emotion(2, 2026, "我心里很平静"),
        ];

        let evolution =
            intervals_for_dimension(&facts, FactType::Emotion, &["content", "emotion", "label"])
                .expect("production emotion facts must map to the emotion dimension");
        assert_eq!(
            evolution.key, "emotion",
            "the dimension key is the fact type name"
        );
        assert_eq!(
            evolution.intervals.len(),
            2,
            "both emotion states survive as intervals"
        );
        assert_eq!(
            evolution.intervals[0].from, 2024,
            "the first interval starts at the earliest state"
        );
        assert!(
            evolution.intervals[1].to.is_none(),
            "the latest interval is still open"
        );
    }
}
