//! Memory reconciliation decision engine (mem0 v3 ADD-only, no LLM).
//!
//! This module implements the "harmonization" step that mem0 v3 performs
//! before storing a candidate fact. The v3 design is ADD-only: facts
//! accumulate, nothing is overwritten or deleted. The reconciler's job is
//! to decide, for each candidate fact, whether it is a genuinely new fact
//! ([`ReconcileDecision::Add`]), a semantic duplicate of an existing fact
//! ([`ReconcileDecision::Noop`]), or a stance-flip transition that should be
//! stored with a transition marker ([`ReconcileDecision::Add`] with
//! [`TransitionMeta`]).
//!
//! Unlike the mem0 v2 LLM tool-call approach, this reconciler is a pure
//! embedding-similarity + rule computation:
//!
//! 1. Retrieve the top-s most semantically similar existing facts for the
//!    same entity (callers pass these in; the reconciler does not touch the
//!    store).
//! 2. Compare the candidate against each retrieved fact.
//!    - If the negation differs AND cosine severity ≥ [`PersonaThresholds::conflict`]
//!      → Add with a stance-flip transition marker. A reversal is a real change
//!      and is always recorded, even at dedup-level similarity.
//!    - Else if cosine similarity ≥ [`PersonaThresholds::dedup`] → NOOP (true
//!      duplicate, same negation).
//!    - Otherwise → Add (genuinely new).
//!
//! All three non-duplicate outcomes are ADDs; v3 never deletes. The
//! transition marker is metadata on the new fact, not a mutation of the old
//! one, so the "persona evolution timeline" can be rebuilt at any time.

use crate::cognition::Fact;
use crate::persona::prototype::PersonaThresholds;

/// The reconciler's verdict for one candidate fact.
///
/// All variants except [`Self::Noop`] result in the candidate being stored;
/// the difference is whether transition metadata is attached.
#[derive(Debug, Clone, PartialEq)]
pub enum ReconcileDecision {
    /// Store the candidate as a new fact (v3 accumulation).
    Add,
    /// Store the candidate, but mark it as a stance-flip transition from
    /// the referenced existing fact. The existing fact is **not** modified.
    AddWithTransition(TransitionMeta),
    /// Skip the candidate — it is a semantic duplicate of an existing fact.
    Noop,
}

/// Transition metadata attached to a stance-flip fact.
///
/// `transition_from` is the id of the existing fact that this new fact
/// supersedes/contradicts. The existing fact remains in the store (v3
/// ADD-only), but the reconciler flags the relationship so the timeline
/// layer can reconstruct "before → after" arcs.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionMeta {
    /// The id of the existing fact this transition supersedes.
    pub transition_from: i64,
    /// The type of transition.
    pub transition_type: TransitionType,
    /// Cosine similarity between the candidate and the superseded fact.
    pub similarity: f32,
}

/// The kind of stance-flip detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionType {
    /// A negated statement now affirms what was previously denied, or vice
    /// versa (e.g. "我喜欢纽约" → "我讨厌纽约").
    StanceFlip,
}

impl TransitionType {
    /// Returns the string representation stored in fact payloads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TransitionType::StanceFlip => "stance_flip",
        }
    }
}

/// Configuration for the reconciler.
///
/// Currently just wraps the tuned thresholds; kept as a struct so additional
/// knobs (e.g. top-s retrieval count) can be added without breaking callers.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ReconcilerConfig {
    /// Tunable cosine-similarity thresholds.
    pub thresholds: PersonaThresholds,
}

/// The outcome of reconciling a single candidate fact.
///
/// Carries the decision plus the winning similarity that drove it, so callers
/// can log or attach evidence without re-searching.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconcileOutcome {
    /// The reconciler's verdict.
    pub decision: ReconcileDecision,
    /// The winning cosine similarity: the dedup/transition similarity for
    /// Noop / AddWithTransition, or `0.0` for a plain Add.
    pub similarity: f32,
}

/// The memory reconciliation engine.
///
/// Stateless and synchronous: callers retrieve the candidate's nearest
/// existing facts (by whatever vector store they use) and pass them in. The
/// reconciler never touches storage directly — it only computes decisions.
pub struct Reconciler {
    config: ReconcilerConfig,
}

impl Reconciler {
    /// Construct a new reconciler with the given config.
    #[must_use]
    pub const fn new(config: ReconcilerConfig) -> Self {
        Self { config }
    }

    /// Reconcile a candidate fact against its nearest existing facts.
    ///
    /// # Arguments
    ///
    /// * `candidate` - The new fact to evaluate.
    /// * `candidate_embedding` - The candidate's embedding vector.
    /// * `existing` - The pre-retrieved nearest existing facts for the same
    ///   entity, each paired with its embedding.
    ///
    /// # Returns
    ///
    /// A [`ReconcileOutcome`] containing the decision and the winning
    /// similarity. If `existing` is empty the decision is always `Add`.
    ///
    /// The function does not error: every input yields a decision.
    pub fn reconcile(
        &self,
        candidate: &Fact,
        candidate_embedding: &[f32],
        existing: &[(&Fact, &[f32])],
    ) -> ReconcileOutcome {
        let dedup = self.config.thresholds.dedup;
        let conflict = self.config.thresholds.conflict;

        // A stance change (opposite negation) is always worth recording, even
        // at dedup-level similarity — the reversal IS the change. So Noop
        // (duplicate) is only triggered when the negation matches (a true
        // duplicate with nothing new to store).
        let candidate_neg = extract_negated(candidate);
        let mut best_dup: Option<f32> = None;
        let mut best_transition: Option<(i64, f32)> = None;

        for (fact, embedding) in existing {
            let sim = cosine(candidate_embedding, embedding);
            let existing_neg = extract_negated(fact);
            let negated_differs =
                candidate_neg.is_some() && existing_neg.is_some() && candidate_neg != existing_neg;

            if negated_differs {
                // Stance-flip: high similarity + opposite negation → record it,
                // even if cosine ≥ dedup.
                if sim >= conflict && best_transition.is_none_or(|(_, s)| sim > s) {
                    if let Some(id) = fact.id {
                        best_transition = Some((id, sim));
                    }
                }
            } else if sim >= dedup && best_dup.is_none_or(|b| sim > b) {
                // True duplicate (same negation): nothing new to store.
                best_dup = Some(sim);
            }
        }

        // Transition (stance change) takes priority over duplicate: a reversal
        // is a real change worth recording, never a no-op.
        if let Some((from_id, sim)) = best_transition {
            return ReconcileOutcome {
                decision: ReconcileDecision::AddWithTransition(TransitionMeta {
                    transition_from: from_id,
                    transition_type: TransitionType::StanceFlip,
                    similarity: sim,
                }),
                similarity: sim,
            };
        }

        if let Some(sim) = best_dup {
            return ReconcileOutcome {
                decision: ReconcileDecision::Noop,
                similarity: sim,
            };
        }

        ReconcileOutcome {
            decision: ReconcileDecision::Add,
            similarity: 0.0,
        }
    }
}

/// Cosine similarity in `[0.0, 1.0]` for same-length nonzero vectors.
///
/// Returns `0.0` on dimension mismatch or zero-magnitude vectors so the
/// caller treats them as maximally dissimilar (no false duplicates).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        return 0.0;
    }
    (dot / denom).clamp(0.0, 1.0)
}

/// Extract the `negated` flag from a fact's payload, if present.
///
/// Persona facts carry `{"negated": bool}` in their payload. This helper
/// reads it without panicking on malformed payloads.
fn extract_negated(fact: &Fact) -> Option<bool> {
    fact.payload
        .get("negated")
        .and_then(serde_json::Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::FactType;
    use crate::persona::prototype::PersonaThresholds;

    fn make_fact(id: i64, fact_type: FactType, negated: bool, content: &str) -> Fact {
        Fact {
            id: Some(id),
            entity_id: 1,
            fact_type,
            time: 1,
            payload: serde_json::json!({
                "content": content,
                "negated": negated,
            }),
            evidence_id: None,
            created_at: 1,
            ..Fact::default()
        }
    }

    fn thresholds() -> PersonaThresholds {
        PersonaThresholds {
            match_: 0.75,
            dedup: 0.95,
            conflict: 0.75,
        }
    }

    /// Objective: Verify a candidate with no existing facts is always Add.
    /// Invariants: empty existing → Add decision.
    #[test]
    fn no_existing_facts_adds() {
        let reconciler = Reconciler::new(ReconcilerConfig {
            thresholds: thresholds(),
        });
        let candidate = make_fact(10, FactType::Identity, false, "我是白流苏");
        let outcome = reconciler.reconcile(&candidate, &[1.0, 0.0], &[]);
        assert_eq!(
            outcome.decision,
            ReconcileDecision::Add,
            "no existing → Add"
        );
    }

    /// Objective: Verify a near-identical duplicate (sim ≥ dedup) is Noop.
    /// Invariants: sim=0.99 ≥ dedup(0.95) → Noop.
    #[test]
    fn duplicate_above_dedup_is_noop() {
        let reconciler = Reconciler::new(ReconcilerConfig {
            thresholds: thresholds(),
        });
        let candidate = make_fact(10, FactType::Identity, false, "我是白流苏");
        let existing = make_fact(1, FactType::Identity, false, "我是白流苏");
        let existing_embedding = vec![0.99_f32, 0.01];
        let candidate_embedding = vec![1.0_f32, 0.0];
        let outcome = reconciler.reconcile(
            &candidate,
            &candidate_embedding,
            &[(&existing, &existing_embedding)],
        );
        assert_eq!(
            outcome.decision,
            ReconcileDecision::Noop,
            "near-duplicate → Noop"
        );
    }

    /// Objective: Verify a stance-flip (opposite negated, sim ≥ conflict)
    /// produces AddWithTransition.
    /// Invariants: candidate negated=true, existing negated=false,
    /// sim=0.85 ≥ conflict(0.75) → AddWithTransition with StanceFlip.
    #[test]
    fn stance_flip_produces_transition() {
        let reconciler = Reconciler::new(ReconcilerConfig {
            thresholds: thresholds(),
        });
        let candidate = make_fact(10, FactType::Preference, true, "我讨厌应酬");
        let existing = make_fact(1, FactType::Preference, false, "我喜欢应酬");
        // cos([1,0],[0.8,0.6]) = 0.8 — in [conflict, dedup) = [0.75, 0.95).
        let existing_embedding = vec![0.8_f32, 0.6];
        let candidate_embedding = vec![1.0_f32, 0.0];
        let outcome = reconciler.reconcile(
            &candidate,
            &candidate_embedding,
            &[(&existing, &existing_embedding)],
        );
        match outcome.decision {
            ReconcileDecision::AddWithTransition(meta) => {
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
            other => panic!("expected AddWithTransition, got {other:?}"),
        }
    }

    /// Objective: Verify that a genuinely new fact (low similarity to all
    /// existing) is Add with no transition.
    /// Invariants: sim=0.3 < conflict(0.75) → Add, similarity=0.0.
    #[test]
    fn genuinely_new_fact_adds_without_transition() {
        let reconciler = Reconciler::new(ReconcilerConfig {
            thresholds: thresholds(),
        });
        let candidate = make_fact(10, FactType::Goal, false, "我要活下去");
        let existing = make_fact(1, FactType::Identity, false, "我是白流苏");
        // Low similarity
        let existing_embedding = vec![0.3_f32, 0.7];
        let candidate_embedding = vec![1.0_f32, 0.0];
        let outcome = reconciler.reconcile(
            &candidate,
            &candidate_embedding,
            &[(&existing, &existing_embedding)],
        );
        assert_eq!(
            outcome.decision,
            ReconcileDecision::Add,
            "low similarity → Add without transition"
        );
    }

    /// Objective: Verify a stance-flip (opposite negation) is recorded even
    /// at dedup-level similarity — the reversal takes priority over the
    /// duplicate check.
    /// Invariants: sim ≈ 0.9998 ≥ dedup AND negated differs → AddWithTransition,
    /// not Noop.
    #[test]
    fn transition_takes_priority_over_duplicate() {
        let reconciler = Reconciler::new(ReconcilerConfig {
            thresholds: thresholds(),
        });
        let candidate = make_fact(10, FactType::Preference, true, "我讨厌应酬");
        let existing = make_fact(1, FactType::Preference, false, "我喜欢应酬");
        // Sim ≈ 0.9998 — above dedup, opposite negated.
        let existing_embedding = vec![0.99_f32, 0.01];
        let candidate_embedding = vec![1.0_f32, 0.0];
        let outcome = reconciler.reconcile(
            &candidate,
            &candidate_embedding,
            &[(&existing, &existing_embedding)],
        );
        match outcome.decision {
            ReconcileDecision::AddWithTransition(meta) => {
                assert_eq!(
                    meta.transition_from, 1,
                    "stance-flip recorded even at dedup similarity"
                );
            }
            other => panic!("expected AddWithTransition, got {other:?}"),
        }
    }

    /// Objective: Verify cosine returns 0.0 for dimension mismatch instead
    /// of panicking.
    /// Invariants: cosine([1,2,3], [1,2]) → 0.0.
    #[test]
    fn cosine_dimension_mismatch_returns_zero() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![1.0_f32, 2.0];
        let s = cosine(&a, &b);
        assert_eq!(s, 0.0, "dimension mismatch → 0.0");
    }

    /// Objective: Verify cosine returns 0.0 for zero-magnitude vectors.
    /// Invariants: cosine(zero, nonzero) → 0.0.
    #[test]
    fn cosine_zero_magnitude_returns_zero() {
        let zero: Vec<f32> = vec![0.0, 0.0, 0.0];
        let nonzero = vec![1.0_f32, 2.0, 3.0];
        let s = cosine(&zero, &nonzero);
        assert_eq!(s, 0.0, "zero-magnitude → 0.0");
    }

    /// Objective: Verify identical vectors yield cosine ≈ 1.0.
    /// Invariants: cosine(v, v) ≈ 1.0.
    #[test]
    fn cosine_identical_vectors() {
        let v = vec![1.0_f32, 2.0, 3.0];
        let s = cosine(&v, &v);
        assert!((s - 1.0).abs() < 1e-5, "identical vectors → cosine ≈ 1.0");
    }

    /// Objective: Verify extract_negated reads the flag from the payload.
    /// Invariants: payload {"negated": true} → Some(true).
    #[test]
    fn extract_negated_reads_payload() {
        let fact = make_fact(1, FactType::Preference, true, "我讨厌应酬");
        assert_eq!(extract_negated(&fact), Some(true), "negated flag extracted");
    }

    /// Objective: Verify extract_negated returns None for payloads without
    /// the flag.
    /// Invariants: payload without "negated" → None.
    #[test]
    fn extract_negated_missing_returns_none() {
        let fact = Fact {
            id: Some(1),
            entity_id: 1,
            fact_type: FactType::Identity,
            time: 1,
            payload: serde_json::json!({"content": "我是白流苏"}),
            evidence_id: None,
            created_at: 1,
            ..Fact::default()
        };
        assert_eq!(extract_negated(&fact), None, "missing negated → None");
    }

    /// Objective: Verify TransitionType::as_str returns the correct string.
    /// Invariants: StanceFlip → "stance_flip".
    #[test]
    fn transition_type_as_str() {
        assert_eq!(
            TransitionType::StanceFlip.as_str(),
            "stance_flip",
            "StanceFlip → 'stance_flip'"
        );
    }
}
