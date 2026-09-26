//! Cognitive State History — the "how did the current state emerge?" layer.
//!
//! State history turns `StateEngine` from a latest-wins aggregator into a state
//! evolution engine. Alongside `aggregate()` ("what is the current state?"),
//! [`aggregate_intervals`] answers "what was true before, and how did it
//! change?".
//!
//! Design rules (frozen; see `docs/zh/dev-plan-cognitive-state.md`):
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

/// Payload fields that identify the *topic* two states have in common.
///
/// A differing state value is a `GradualChange` only when the two payloads
/// agree on one of these fields: the change then happens *within* one topic
/// (same action/theme) instead of being two unrelated facts.
///
/// `content` is deliberately absent — it is the state value itself, so it can
/// never be the shared part. So are `attribution` (a channel marker) and
/// `subject` (constant per channel): both would make every pair of facts look
/// like one topic.
const TOPIC_KEYS: &[&str] = &["keyword", "topic", "preference", "action"];

/// A state-validity window over one semantic key.
///
/// `from`/`to` are **state validity times**, not observation times (§2.2 of
/// the plan). `to == None` means the state still holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateInterval {
    /// When this state became valid.
    pub from: i64,
    /// When this state ceased to be valid (`None` = still current).
    pub to: Option<i64>,
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
    pub at: i64,
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
        let negated = fact.negated();
        let fold_key = (state_value.clone(), negated);
        if let Some(interval) = &mut current {
            if interval.fold_key == fold_key {
                // Same state — extend the interval.
                interval.fact_ids.extend(fact.id);
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
            // Only real ids: an unsaved fact must not be reported as evidence
            // link `0`, which no row can ever satisfy.
            fact_ids: fact.id.into_iter().collect(),
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

/// Resolve the fold value of a fact: the first present *string* `value_keys`
/// field, or the whole payload when none is present.
///
/// Only string values count, matching
/// [`latest_by_payload_key`](crate::cognition::StateEngine) — the current-state
/// projection buckets facts by exactly the same rule, so the two views can
/// never disagree about which facts describe the same state.
///
/// Falling back to the payload (rather than `Null`) keeps key-less facts
/// distinct — with a `Null` fallback every such fact folded into a single
/// interval and all but the last state silently disappeared.
fn resolve_state_value(payload: &serde_json::Value, value_keys: &[&str]) -> serde_json::Value {
    value_keys
        .iter()
        .find_map(|key| payload.get(*key).and_then(serde_json::Value::as_str))
        .map_or_else(
            || payload.clone(),
            |value| serde_json::Value::String(value.to_owned()),
        )
}

/// Deterministic transition detection between two consecutive intervals.
///
/// Returns `None` when no definite relation exists — the change is then
/// reported as intervals only (allowed to be uncertain).
///
/// The three signals are ranked strongest-first: an explicit negation flip
/// (`StanceFlip`) outranks an intent-then-action pair
/// (`BehavioralConfirmation`), which outranks a change of value inside one
/// shared topic (`GradualChange`).
fn detect_transition(from: &StateInterval, to: &StateInterval) -> Option<TransitionType> {
    let from_text = state_text(&from.value);
    let to_text = state_text(&to.value);
    if from_text.is_empty() || to_text.is_empty() {
        return None;
    }

    // StanceFlip: same topic, opposite negation. Bigram overlap alone is too
    // loose across formulaic templates ("我喜欢和你聊天" vs "我不喜欢和你
    // 吵架" share 喜欢/欢和/和你) — when BOTH payloads carry a topic-bearing
    // key, require them to agree after stripping stance verbs that some
    // fixtures embed in the preference value itself ("喜欢应酬" vs
    // "不喜欢应酬" are the same topic 应酬).
    if let (Some(from_negated), Some(to_negated)) =
        (negation_of(&from.value), negation_of(&to.value))
    {
        if from_negated != to_negated
            && shared_bigrams(from_text, to_text) >= STANCE_FLIP_MIN_SHARED_BIGRAMS
            && (!both_have_topic_fields(&from.value, &to.value)
                || shares_normalized_topic(&from.value, &to.value))
        {
            return Some(TransitionType::StanceFlip);
        }
    }

    // BehavioralConfirmation: the later state's text is about action while the
    // earlier was still intent/preference (not itself an action). Two already-
    // performed actions ("参加了训练" → "参加了比赛") are NOT a confirmation;
    // an intent marker OR a non-action earlier state qualifies the transition.
    if shared_bigrams(from_text, to_text) >= STANCE_FLIP_MIN_SHARED_BIGRAMS
        && contains_action_word(to_text)
        && (contains_intent_word(from_text) || !contains_action_word(from_text))
    {
        return Some(TransitionType::BehavioralConfirmation);
    }

    // GradualChange: same topic, different value.
    //
    // The shared-topic test is what keeps this conservative, and it must be
    // `TOPIC_KEYS`-based rather than `content`-based: `content` is the state
    // value itself. Requiring two fields that production never emits together
    // (an earlier revision demanded `keyword` while the comparison text came
    // from `content`) made the plan's canonical "喜欢独处 → 开始想社交 →
    // 喜欢热闹" chain unreachable on every real compile path.
    if from_text != to_text && shares_topic(&from.value, &to.value) {
        return Some(TransitionType::GradualChange);
    }

    None
}

/// Read the negation flag out of an interval value, when it carries one.
fn negation_of(value: &serde_json::Value) -> Option<bool> {
    value.get("negated").and_then(serde_json::Value::as_bool)
}

/// The comparable text of a state, read from the first human-readable field.
///
/// Production payloads disagree on the field name (`content` for the user and
/// persona channels, `object`/`action` for observation rules, `keyword` for
/// companion themes), so the comparison text has one fallback chain instead of
/// reading `content` only.
fn state_text(value: &serde_json::Value) -> &str {
    ["content", "object", "keyword", "action"]
        .iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
        .unwrap_or("")
}

/// True when both payloads carry at least one topic-bearing key (so the
/// shared-topic guard is meaningful rather than vacuously true).
fn both_have_topic_fields(from: &serde_json::Value, to: &serde_json::Value) -> bool {
    TOPIC_KEYS.iter().any(|key| {
        from.get(*key).and_then(serde_json::Value::as_str).is_some()
            && to.get(*key).and_then(serde_json::Value::as_str).is_some()
    })
}

/// Like [`shares_topic`], but strips stance verbs embedded in topic values
/// before comparing ("喜欢应酬" and "不喜欢应酬" both normalize to "应酬").
fn shares_normalized_topic(from: &serde_json::Value, to: &serde_json::Value) -> bool {
    TOPIC_KEYS.iter().any(|key| {
        matches!(
            (
                from.get(*key).and_then(serde_json::Value::as_str).map(normalize_stance_topic),
                to.get(*key).and_then(serde_json::Value::as_str).map(normalize_stance_topic)
            ),
            (Some(a), Some(b)) if !a.is_empty() && a == b
        )
    })
}

/// Strip common stance verbs so a preference value that embeds the stance
/// still yields the underlying topic for flip detection.
///
/// If stripping empties the string (e.g. the whole value IS "喜欢"), keep the
/// original — an empty topic must never compare equal by accident, and two
/// identical `action:"喜欢"` markers are the same topic.
fn normalize_stance_topic(s: &str) -> String {
    let stripped = s
        .trim_start_matches("不喜欢")
        .trim_start_matches("讨厌")
        .trim_start_matches("喜欢")
        .trim_start_matches("不")
        .trim();
    if stripped.is_empty() {
        s.to_string()
    } else {
        stripped.to_string()
    }
}

/// True when both payloads agree on at least one [`TOPIC_KEYS`] field.
fn shares_topic(from: &serde_json::Value, to: &serde_json::Value) -> bool {
    TOPIC_KEYS.iter().any(|key| {
        matches!(
            (
                from.get(*key).and_then(serde_json::Value::as_str),
                to.get(*key).and_then(serde_json::Value::as_str)
            ),
            (Some(from_value), Some(to_value)) if from_value == to_value
        )
    })
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

/// Intent markers required in the EARLIER state for BehavioralConfirmation:
/// the doc contract is "stated intent later confirmed by behavior".
fn contains_intent_word(content: &str) -> bool {
    [
        "想", "要", "计划", "打算", "希望", "准备", "want", "plan", "hope", "will",
    ]
    .iter()
    .any(|word| content.contains(word))
}

/// One cognitive dimension: which facts belong to it and how they are keyed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CognitiveDimension {
    /// The fact type that belongs to this dimension.
    pub fact_type: FactType,
    /// Payload fields naming the *state value* — the thing that changes
    /// ("喜欢 Python" → "开始喜欢 Rust"). The history layer folds a dimension
    /// by this key; the first present string field wins and the whole payload is
    /// the fallback. A trailing key must stay **stable** over time: a counter
    /// such as companion themes' `occurrences` changing on every compile would
    /// otherwise split one state into an interval per observation.
    pub value_keys: &'static [&'static str],
    /// Payload fields naming the *topic* of the dimension. The current state
    /// keeps one latest entry per topic, so its priority is topic-first.
    ///
    /// Same vocabulary as `value_keys`, deliberately different priority: the
    /// history answers "how did this value change?" (so `content` leads), while
    /// the current state answers "what is true per topic?" (so `topic` leads).
    /// Keeping both lists in one table is what stops them from drifting apart.
    pub topic_keys: &'static [&'static str],
}

/// The five cognitive dimensions both projections report on.
///
/// Facts always belong to a dimension by [`FactType`] — mirroring
/// [`StateEngine::aggregate`](crate::cognition::StateEngine::aggregate) — never
/// by the presence of a payload field, so the current state and the state
/// history can never disagree about which facts a dimension contains.
pub const COGNITIVE_DIMENSIONS: &[CognitiveDimension] = &[
    CognitiveDimension {
        fact_type: FactType::Goal,
        value_keys: &["content", "goal", "keyword"],
        topic_keys: &["goal", "content", "keyword"],
    },
    CognitiveDimension {
        fact_type: FactType::Preference,
        value_keys: &["content", "preference", "topic", "keyword"],
        topic_keys: &["topic", "preference", "content", "keyword"],
    },
    CognitiveDimension {
        fact_type: FactType::Emotion,
        // Emotion key FIRST: payloads from companion_extract carry
        // `content` (the per-observation quote) plus `emotion` (the label).
        // Leading with content split every repeated observation of the SAME
        // emotion into its own interval (phantom splits); leading with the
        // label folds "烦死了" → "我好烦" into one continuous state. Payloads
        // without `emotion`/`label` still fall back to `content`.
        value_keys: &["emotion", "label", "content", "keyword"],
        topic_keys: &["emotion", "label", "content", "keyword"],
    },
    CognitiveDimension {
        fact_type: FactType::Relationship,
        value_keys: &["content", "target", "with", "object", "keyword"],
        topic_keys: &["target", "with", "object", "content", "keyword"],
    },
    CognitiveDimension {
        fact_type: FactType::Identity,
        value_keys: &["content", "identity", "attribute", "key", "keyword"],
        topic_keys: &["attribute", "identity", "key", "content", "keyword"],
    },
];

/// The dimension `fact_type` belongs to, when it is a cognitive dimension.
#[must_use]
pub fn cognitive_dimension(fact_type: FactType) -> Option<&'static CognitiveDimension> {
    COGNITIVE_DIMENSIONS
        .iter()
        .find(|dimension| dimension.fact_type == fact_type)
}

/// The history fold keys of `fact_type` (see [`CognitiveDimension::value_keys`]).
///
/// Types outside the five cognitive dimensions fall back to `content`.
#[must_use]
pub fn dimension_value_keys(fact_type: FactType) -> &'static [&'static str] {
    cognitive_dimension(fact_type).map_or(&["content"], |dimension| dimension.value_keys)
}

/// The current-state topic keys of `fact_type` (see
/// [`CognitiveDimension::topic_keys`]).
///
/// Types outside the five cognitive dimensions fall back to `content`.
#[must_use]
pub fn dimension_topic_keys(fact_type: FactType) -> &'static [&'static str] {
    cognitive_dimension(fact_type).map_or(&["content"], |dimension| dimension.topic_keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::FactType;

    fn fact(
        id: i64,
        fact_type: FactType,
        time: i64,
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
            created_at: time,
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

    /// Objective: Verify production-shaped facts still map onto their cognitive
    /// dimension. Facts emitted by the companion channels carry only
    /// `attribution`/`content`/`negated`; the old payload-key filter required a
    /// field such as `emotion`, so `state_timeline` returned zero dimensions for
    /// every real conversation.
    /// Invariants: two Emotion facts yield the `emotion` dimension with two
    /// intervals (ADD-only: neither state is folded away).
    #[test]
    fn production_shaped_payloads_map_to_their_dimension() {
        let emotion = |id: i64, time: i64, content: &str| Fact {
            id: Some(id),
            entity_id: 7,
            fact_type: FactType::Emotion,
            time,
            payload: serde_json::json!({
                "attribution": "agent_personality",
                "content": content,
                "negated": false,
            }),
            created_at: time,
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

    /// Objective: Verify a companion theme folds on its stable `keyword` rather
    /// than on the whole payload. `occurrences` grows on every compile, so a
    /// payload-wide fold key reported one fresh "state" per observation and the
    /// timeline filled up with phantom changes.
    /// Invariants: two theme facts with the same keyword and different counters
    /// fold into ONE interval carrying both fact ids.
    #[test]
    fn companion_theme_facts_fold_on_their_keyword() {
        let theme = |id: i64, time: i64, occurrences: i64| Fact {
            id: Some(id),
            entity_id: 7,
            fact_type: FactType::Preference,
            time,
            payload: serde_json::json!({
                "keyword": "露营",
                "occurrences": occurrences,
                "samples": ["周末去露营"],
            }),
            created_at: time,
            ..Fact::default()
        };
        let evolution = intervals_for_dimension(
            &[theme(1, 2024, 1), theme(2, 2026, 3)],
            FactType::Preference,
            dimension_value_keys(FactType::Preference),
        )
        .expect("the preference dimension has facts");
        assert_eq!(
            evolution.intervals.len(),
            1,
            "a growing occurrence counter is the SAME state, not a new one"
        );
        assert_eq!(
            evolution.intervals[0].fact_ids,
            vec![1, 2],
            "both observations belong to that single state"
        );
    }

    /// Objective: Verify an interval never claims a fabricated evidence link.
    /// Invariants: a fact without an id yields an empty `fact_ids` (the previous
    /// `unwrap_or(0)` published fact id `0`, which no row can satisfy).
    #[test]
    fn unsaved_facts_never_claim_a_fabricated_id() {
        let unsaved = Fact {
            id: None,
            entity_id: 7,
            fact_type: FactType::Goal,
            time: 2026,
            payload: serde_json::json!({"content": "我要学 Rust"}),
            created_at: 2026,
            ..Fact::default()
        };
        let evolution = intervals_for_dimension(
            &[unsaved],
            FactType::Goal,
            dimension_value_keys(FactType::Goal),
        )
        .expect("the goal dimension has facts");
        assert!(
            evolution.intervals[0].fact_ids.is_empty(),
            "an unsaved fact must not be reported as fact id 0"
        );
    }

    /// Objective: Verify the key list has a single source, so the current-state
    /// projection and the state history can never bucket one dimension two
    /// different ways.
    /// Invariants: the helper returns the table entry for every cognitive
    /// dimension, `content` leads each list, and a non-dimension type falls back
    /// to `content`.
    #[test]
    fn dimension_keys_have_a_single_source() {
        for dimension in COGNITIVE_DIMENSIONS {
            let fact_type = dimension.fact_type;
            assert_eq!(
                dimension_value_keys(fact_type),
                dimension.value_keys,
                "the history fold keys must come from the table for {fact_type:?}"
            );
            assert_eq!(
                dimension_topic_keys(fact_type),
                dimension.topic_keys,
                "the current-state topic keys must come from the table for {fact_type:?}"
            );
            assert_eq!(
                dimension.value_keys.first().copied(),
                // Emotion deliberately leads with the label so repeated
                // observations of the same emotion fold into one interval;
                // every other dimension still leads with `content`.
                if dimension.fact_type == FactType::Emotion {
                    Some("emotion")
                } else {
                    Some("content")
                },
                "history fold keys start with the stable value field for {fact_type:?}",
                fact_type = dimension.fact_type
            );
            // Both lists must speak the same vocabulary: a topic key the
            // history does not know would let the two projections disagree
            // about which facts describe the same state.
            for key in dimension.topic_keys {
                assert!(
                    dimension.value_keys.contains(key),
                    "{fact_type:?} topic key `{key}` is unknown to the history fold"
                );
            }
        }
        let expected: &[&str] = &["content"];
        assert_eq!(
            dimension_value_keys(FactType::Event),
            expected,
            "value keys of a non-dimension type fall back to content"
        );
        assert_eq!(
            dimension_topic_keys(FactType::Event),
            expected,
            "topic keys of a non-dimension type fall back to content"
        );
    }
}
