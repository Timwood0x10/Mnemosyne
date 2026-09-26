//! End-to-end integration tests for the persona extraction → attribution →
//! reconciliation → fact construction pipeline.
//!
//! These tests exercise the full P1-P7 chain with a stub embedder:
//! 1. [`KeywordPersonaExtractor`] extracts a signal from an utterance.
//! 2. [`SpeakerAttribution`] maps the speaker to the target entity.
//! 3. [`signal_to_fact`] builds a [`Fact`] with the persona attribution
//!    marker and `negated` flag.
//! 4. [`Reconciler`] compares the candidate against existing facts and
//!    returns ADD / NOOP / AddWithTransition.
//!
//! The three acceptance points verified here:
//! - **Duplicate recognition (NOOP)**: a near-identical existing fact blocks
//!   the candidate.
//! - **Stance-flip transition marking**: an opposite-negated, high-similarity
//!   candidate is stored with a `TransitionMeta` referencing the superseded
//!   fact (v3 ADD-only: the old fact is not deleted).
//! - **Time-context preservation**: both the old and new facts coexist in
//!   the store with their original `time` and `created_at`, so the
//!   "persona evolution timeline" can be rebuilt.

#![cfg(test)]

use crate::cognition::{Fact, FactType};
use crate::persona::PersonaSignalExtractor;
use crate::persona::keyword_extractor::KeywordPersonaExtractor;
use crate::persona::reconciler::{ReconcileDecision, Reconciler, ReconcilerConfig, TransitionType};
use crate::persona::signal_to_fact;
use crate::persona::speaker_attribution::{Speaker, SpeakerAttribution};

/// Build a persona fact from a raw utterance, simulating the full
/// extraction → attribution → fact-construction pipeline.
async fn utterance_to_fact(
    utterance: &str,
    speaker: Speaker,
    entity_id: i64,
    logical_time: i64,
) -> Option<Fact> {
    let extractor = KeywordPersonaExtractor::new();
    let signals = extractor
        .extract(utterance)
        .await
        .expect("keyword extraction");
    if signals.is_empty() {
        return None;
    }
    let sig = &signals[0];
    let attribution = SpeakerAttribution::new(speaker, entity_id);
    Some(signal_to_fact(sig, &attribution, logical_time))
}

/// Objective: Verify the end-to-end pipeline extracts an identity statement,
/// attributes it to the agent entity, and builds a fact with the correct
/// payload fields.
/// Invariants: "我是白流苏" → Fact with fact_type=Identity, negated=false,
/// speaker="agent", attribution marker present.
#[tokio::test]
async fn pipeline_extracts_and_attributes_identity() {
    let fact = utterance_to_fact(
        "我是白流苏，离过婚，爱过，也输过。",
        Speaker::Agent,
        42,
        100,
    )
    .await
    .expect("identity signal extracted");
    assert_eq!(fact.entity_id, 42, "attributed to agent entity 42");
    assert_eq!(fact.fact_type, FactType::Identity, "classified as Identity");
    assert_eq!(fact.time, 100, "logical time preserved");
    assert_eq!(
        fact.payload["speaker"], "agent",
        "speaker field set to agent"
    );
    assert_eq!(
        fact.payload["negated"], false,
        "affirmative identity not negated"
    );
    assert!(
        fact.payload["attribution"]
            .as_str()
            .is_some_and(|s| s == "agent_personality"),
        "persona attribution marker present"
    );
}

/// Objective: Verify the reconciler recognises a semantic duplicate (NOOP)
/// in the end-to-end pipeline.
/// Invariants: Two near-identical identity facts → second is Noop.
#[tokio::test]
async fn pipeline_duplicate_recognition_noop() {
    let config = ReconcilerConfig::default();
    let reconciler = Reconciler::new(config);

    // First fact: stored at t=100.
    let existing = utterance_to_fact(
        "我是白流苏，离过婚，爱过，也输过。",
        Speaker::Agent,
        42,
        100,
    )
    .await
    .expect("first identity extracted");

    // Second fact: same content, slightly later time. In a real system the
    // embeddings would be near-identical (cosine ≈ 0.99).
    let candidate = utterance_to_fact(
        "我是白流苏，离过婚，爱过，也输过。",
        Speaker::Agent,
        42,
        200,
    )
    .await
    .expect("second identity extracted");

    // Simulate near-identical embeddings.
    let existing_emb = vec![1.0_f32, 0.0_f32, 0.0_f32];
    let candidate_emb = vec![0.99_f32, 0.01_f32, 0.0_f32];

    let outcome = reconciler.reconcile(&candidate, &candidate_emb, &[(&existing, &existing_emb)]);
    assert_eq!(
        outcome.decision,
        ReconcileDecision::Noop,
        "near-duplicate candidate → Noop"
    );
}

/// Objective: Verify the reconciler marks a stance-flip transition
/// (AddWithTransition) when a negated candidate contradicts an affirmative
/// existing fact with high similarity.
/// Invariants: existing "我喜欢应酬" (negated=false) + candidate
/// "我讨厌应酬" (negated=true), sim ≥ conflict → AddWithTransition with
/// StanceFlip type, transition_from = existing fact's id.
#[tokio::test]
async fn pipeline_stance_flip_transition_marking() {
    let config = ReconcilerConfig::default();
    let reconciler = Reconciler::new(config);

    // Existing affirmative preference.
    let mut existing = utterance_to_fact("我喜欢应酬，热闹。", Speaker::Agent, 42, 100)
        .await
        .expect("affirmative preference extracted");
    existing.id = Some(1);

    // Candidate negated preference — stance-flip.
    let mut candidate = utterance_to_fact("我讨厌应酬，太累了。", Speaker::Agent, 42, 200)
        .await
        .expect("negated preference extracted");
    candidate.id = Some(2);

    // Simulate high-similarity embeddings (both about "应酬") with cosine in
    // [conflict, dedup) = [0.75, 0.95): cos([1,0],[0.8,0.6]) = 0.8.
    let existing_emb = vec![0.8_f32, 0.6_f32, 0.0_f32];
    let candidate_emb = vec![1.0_f32, 0.0_f32, 0.0_f32];

    let outcome = reconciler.reconcile(&candidate, &candidate_emb, &[(&existing, &existing_emb)]);
    match outcome.decision {
        ReconcileDecision::AddWithTransition(ref meta) => {
            assert_eq!(
                meta.transition_from, 1,
                "transition references existing fact id"
            );
            assert_eq!(
                meta.transition_type,
                TransitionType::StanceFlip,
                "stance-flip transition type"
            );
        }
        ref other => panic!("expected AddWithTransition, got {other:?}"),
    }
}

/// Objective: Verify time-context preservation: both the superseded fact
/// and the new transition fact coexist with their original timestamps, so
/// the "persona evolution timeline" can be rebuilt from the store.
/// Invariants: existing.time=100, candidate.time=200; after v3 ADD-only
/// storage both facts remain, ordered by time.
#[tokio::test]
async fn pipeline_time_context_preservation() {
    // Simulate a store as a Vec<Fact> (v3 ADD-only: never remove).
    let mut store: Vec<Fact> = Vec::new();

    // t=100: agent likes socializing.
    let fact_100 = utterance_to_fact("我喜欢应酬，热闹。", Speaker::Agent, 42, 100)
        .await
        .expect("affirmative preference extracted");
    store.push(fact_100);

    // t=200: agent flips stance.
    let fact_200 = utterance_to_fact("我讨厌应酬，太累了。", Speaker::Agent, 42, 200)
        .await
        .expect("negated preference extracted");
    store.push(fact_200);

    // Rebuild timeline: sort by time, verify both facts present.
    let mut timeline = store.clone();
    timeline.sort_by_key(|f| f.time);
    assert_eq!(
        timeline.len(),
        2,
        "v3 ADD-only: both stance-flip facts coexist"
    );
    assert_eq!(timeline[0].time, 100, "earlier fact preserved at t=100");
    assert_eq!(timeline[1].time, 200, "later fact preserved at t=200");

    // Verify the stance flip is detectable from the stored payloads.
    let earlier_negated = timeline[0].payload["negated"]
        .as_bool()
        .expect("earlier negated flag");
    let later_negated = timeline[1].payload["negated"]
        .as_bool()
        .expect("later negated flag");
    assert!(!earlier_negated, "earlier fact is affirmative");
    assert!(later_negated, "later fact is negated (stance flip)");
}

/// Objective: Verify that a genuinely new fact (low similarity to all
/// existing) is Add with no transition in the end-to-end pipeline.
/// Invariants: existing identity + candidate goal (low sim) → Add.
#[tokio::test]
async fn pipeline_genuinely_new_fact_adds() {
    let config = ReconcilerConfig::default();
    let reconciler = Reconciler::new(config);

    let existing = utterance_to_fact("我是白流苏", Speaker::Agent, 42, 100)
        .await
        .expect("identity extracted");

    let candidate = utterance_to_fact("我要活下去", Speaker::Agent, 42, 200)
        .await
        .expect("goal extracted");

    // Low similarity embeddings.
    let existing_emb = vec![1.0_f32, 0.0_f32, 0.0_f32];
    let candidate_emb = vec![0.1_f32, 0.9_f32, 0.0_f32];

    let outcome = reconciler.reconcile(&candidate, &candidate_emb, &[(&existing, &existing_emb)]);
    assert_eq!(
        outcome.decision,
        ReconcileDecision::Add,
        "genuinely new candidate → Add without transition"
    );
}

/// Objective: Verify the pipeline correctly attributes user speech to the
/// user entity (not the agent), supporting mixed-corpus speaker attribution.
/// Invariants: "我喜欢这个建议" from user → Fact with entity_id=user_id,
/// speaker="user".
#[tokio::test]
async fn pipeline_attributes_user_speech_to_user_entity() {
    let fact = utterance_to_fact("我喜欢这个建议", Speaker::User, 99, 100)
        .await
        .expect("user preference extracted");
    assert_eq!(fact.entity_id, 99, "attributed to user entity 99");
    assert_eq!(fact.payload["speaker"], "user", "speaker field set to user");
}
