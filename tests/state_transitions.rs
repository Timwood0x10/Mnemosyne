//! Transition detection over the PUBLIC state-history API.
//!
//! `state_timeline` reports "how did the current state emerge?" as intervals
//! plus typed transitions, so transition detection is an external contract, not
//! an internal detail. These cases live here (rule 4.2) because they only need
//! `intervals_for_dimension` and `dimension_value_keys` — and because two
//! separate rounds of bugs hid in exactly these paths: transitions pinned to the
//! tail window, and a `GradualChange` rule that no production payload could ever
//! satisfy.

use mnemosyne::cognition::{Fact, FactType};
use mnemosyne::state::{TransitionType, dimension_value_keys, intervals_for_dimension};

/// Build a fact carrying `{key: value, content}` plus an optional `negated`
/// flag — the shape the persona channels emit.
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

/// Build a user-channel observation exactly as `conversation_compiler` emits it:
/// `{action, subject, object, content}` with no `keyword`.
fn observation(id: i64, time: i32, action: &str, content: &str) -> Fact {
    Fact {
        id: Some(id),
        entity_id: 7,
        fact_type: FactType::Preference,
        time,
        payload: serde_json::json!({
            "action": action,
            "subject": "user",
            "object": null,
            "content": content,
        }),
        created_at: i64::from(time),
        ..Fact::default()
    }
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

/// Objective: Verify unrelated same-type topics do NOT produce a transition — no
/// shared bigrams and no shared topic → no stance flip (deterministic guard).
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

/// Objective: Verify every transition points at its OWN window once a dimension
/// has three or more intervals. The previous implementation wrote
/// `len() - 2`/`len() - 1`, which pinned all transitions to the tail pair; the
/// two-interval tests never exposed it.
/// Invariants: three states → three intervals and two transitions with
/// `(from_index, to_index)` equal to `(0, 1)` and `(1, 2)`; each referenced pair
/// is adjacent in time (`from.to == to.from`) and moves forward.
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

/// Objective: Verify the plan's canonical gradual change is reachable from
/// PRODUCTION payloads. An earlier revision demanded a `keyword` field for
/// `GradualChange` while reading the comparison text from `content`, but no
/// compiler emits both, so every real conversation reported intervals and zero
/// transitions.
/// Invariants: two observations sharing an `action` but differing in `content`
/// yield one `GradualChange`; two observations differing in both yield none.
#[test]
fn production_observation_shapes_change_gradually() {
    let evolution = intervals_for_dimension(
        &[
            observation(1, 2024, "prefer", "我喜欢独处"),
            observation(2, 2026, "prefer", "我喜欢热闹"),
        ],
        FactType::Preference,
        dimension_value_keys(FactType::Preference),
    )
    .expect("the preference dimension has facts");
    assert_eq!(evolution.intervals.len(), 2, "two distinct states");
    assert_eq!(
        evolution.transitions.len(),
        1,
        "a same-action change must be reported as a transition"
    );
    assert_eq!(
        evolution.transitions[0].transition_type,
        TransitionType::GradualChange
    );

    let unrelated = intervals_for_dimension(
        &[
            observation(1, 2024, "prefer", "我喜欢独处"),
            observation(2, 2026, "feel", "最近压力很大"),
        ],
        FactType::Preference,
        dimension_value_keys(FactType::Preference),
    )
    .expect("the preference dimension has facts");
    assert!(
        unrelated.transitions.is_empty(),
        "unrelated actions must not fabricate a transition"
    );
}

/// Objective: Verify the intent-then-action signal also fires on production
/// payloads, so the reported transition type is not always the fallback.
/// Invariants: a same-action intent followed by a performed action yields exactly
/// one `BehavioralConfirmation`.
#[test]
fn behavioral_confirmation_fires_on_an_observed_action() {
    let evolution = intervals_for_dimension(
        &[
            observation(1, 2024, "prefer", "我想参加社交活动"),
            observation(2, 2026, "prefer", "我今天参加了社交活动"),
        ],
        FactType::Preference,
        dimension_value_keys(FactType::Preference),
    )
    .expect("the preference dimension has facts");
    assert_eq!(evolution.transitions.len(), 1, "one transition");
    assert_eq!(
        evolution.transitions[0].transition_type,
        TransitionType::BehavioralConfirmation
    );
}
