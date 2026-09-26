//! Decision as a first-class cognitive entity (experimental).
//!
//! A [`Decision`] records *what the agent decided, why, and what happened
//! afterwards* — the "Why did the agent act this way?" layer on top of
//! cognitive state. It is deliberately NOT a fact:
//!
//! - `because` references the facts that *supported* the decision (supporting
//!   evidence, not causality: "D was supported by F17", never "F17 caused D").
//! - `outcome` has a lifecycle of its own and starts `None` (the decision is
//!   recorded before the outcome is known). It is never *inferred*: the host
//!   declares it through `memory_compile`'s `decision_outcomes` argument, and
//!   the first outcome recorded for a decision wins.
//!
//! Scope guards (frozen):
//!
//! - A single `decisions` table plus two query APIs — no DecisionGraph,
//!   DecisionPolicy, or DecisionReason abstractions.
//! - This module does NOT touch `memory_decay` semantics: decisions follow
//!   their own entity lifecycle. Whether they should decay is left to future
//!   data-driven decisions.

use serde::{Deserialize, Serialize};

/// The observed outcome of a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    /// The decision was carried out as intended.
    Fulfilled,
    /// The decision was reversed or the commitment broken.
    Violated,
}

impl DecisionOutcome {
    /// Stable lowercase string stored in the `outcome` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionOutcome::Fulfilled => "fulfilled",
            DecisionOutcome::Violated => "violated",
        }
    }

    /// Parse the stored string back into a [`DecisionOutcome`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "fulfilled" => Some(DecisionOutcome::Fulfilled),
            "violated" => Some(DecisionOutcome::Violated),
            _ => None,
        }
    }
}

/// Life-cycle of a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    /// The decision is still open (no outcome recorded yet).
    Open,
    /// The decision has been carried out or closed.
    Closed,
}

impl DecisionStatus {
    /// Stable lowercase string stored in the `status` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionStatus::Open => "open",
            DecisionStatus::Closed => "closed",
        }
    }

    /// Parse the stored string back into a [`DecisionStatus`].
    ///
    /// Unknown values fall back to [`DecisionStatus::Open`] so legacy rows
    /// migrate cleanly.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "closed" => DecisionStatus::Closed,
            _ => DecisionStatus::Open,
        }
    }
}

/// A first-class decision: `subject verb object`, why it was made
/// (`because`), and what happened afterwards (`outcome`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub id: Option<i64>,
    /// Who decided (entity id).
    pub subject: i64,
    /// The action/commitment (e.g. "promise", "decide", "decline").
    pub verb: String,
    /// What the decision was about.
    pub object: String,
    /// When the decision was made (state validity time).
    pub made_at: i64,
    /// Facts that supported the decision. **Supporting evidence, not
    /// causality.**
    pub because: Vec<i64>,
    /// Observed outcome; `None` while the decision is still open.
    pub outcome: Option<DecisionOutcome>,
    pub status: DecisionStatus,
}

/// Validate a decision before persistence.
///
/// Returns the first invalid field name, or `None` when valid.
#[must_use]
pub fn validate_decision(decision: &Decision) -> Option<&'static str> {
    if decision.subject <= 0 {
        return Some("subject");
    }
    if decision.verb.trim().is_empty() {
        return Some("verb");
    }
    if decision.object.trim().is_empty() {
        return Some("object");
    }
    if decision.verb.len() > 64 {
        return Some("verb");
    }
    if decision.object.len() > 512 {
        return Some("object");
    }
    None
}

/// Transition a decision's outcome, flipping `status` to Closed.
///
/// Applying an outcome twice is a no-op that returns the decision unchanged,
/// preserving the first recorded outcome (a decision cannot be both fulfilled
/// and violated).
#[must_use]
pub fn apply_outcome(mut decision: Decision, outcome: DecisionOutcome) -> Decision {
    if decision.outcome.is_some() {
        return decision;
    }
    decision.outcome = Some(outcome);
    decision.status = DecisionStatus::Closed;
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_decision() -> Decision {
        Decision {
            id: None,
            subject: 7,
            verb: "promise".to_string(),
            object: "陪用户明天去医院".to_string(),
            made_at: 2026,
            because: vec![17, 23],
            outcome: None,
            status: DecisionStatus::Open,
        }
    }

    /// Objective: Verify the outcome/status strings round-trip and unknown
    /// strings fall back safely.
    /// Invariants: as_str/parse are inverse for every variant; unknown outcome
    /// → None; unknown status → Open.
    #[test]
    fn enums_roundtrip_and_fall_back_safely() {
        for outcome in [DecisionOutcome::Fulfilled, DecisionOutcome::Violated] {
            assert_eq!(
                DecisionOutcome::parse(outcome.as_str()),
                Some(outcome),
                "outcome round-trips for {outcome:?}"
            );
        }
        assert_eq!(
            DecisionOutcome::parse("nonsense"),
            None,
            "unknown outcome → None"
        );
        for status in [DecisionStatus::Open, DecisionStatus::Closed] {
            assert_eq!(
                DecisionStatus::parse(status.as_str()),
                status,
                "status round-trips for {status:?}"
            );
        }
        assert_eq!(
            DecisionStatus::parse("garbage"),
            DecisionStatus::Open,
            "unknown status → Open"
        );
    }

    /// Objective: Verify validation rejects empty/oversized fields and accepts
    /// a well-formed decision.
    /// Invariants: each invalid field is reported by name; a valid decision
    /// validates as None.
    #[test]
    fn validation_rejects_malformed_decisions() {
        assert_eq!(
            validate_decision(&sample_decision()),
            None,
            "valid decision"
        );

        let mut bad_subject = sample_decision();
        bad_subject.subject = 0;
        assert_eq!(validate_decision(&bad_subject), Some("subject"));

        let mut bad_verb = sample_decision();
        bad_verb.verb = "   ".to_string();
        assert_eq!(validate_decision(&bad_verb), Some("verb"));

        let mut bad_object = sample_decision();
        bad_object.object = String::new();
        assert_eq!(validate_decision(&bad_object), Some("object"));

        let mut long_verb = sample_decision();
        long_verb.verb = "x".repeat(65);
        assert_eq!(validate_decision(&long_verb), Some("verb"));

        let mut long_object = sample_decision();
        long_object.object = "x".repeat(513);
        assert_eq!(validate_decision(&long_object), Some("object"));
    }

    /// Objective: Verify `apply_outcome` records the outcome exactly once and
    /// closes the decision; a second application is a no-op.
    /// Invariants: first outcome wins; status flips Open → Closed; the second
    /// outcome is ignored so a decision can never be both fulfilled/violated.
    #[test]
    fn outcome_is_recorded_exactly_once() {
        let closed = apply_outcome(sample_decision(), DecisionOutcome::Fulfilled);
        assert_eq!(closed.outcome, Some(DecisionOutcome::Fulfilled));
        assert_eq!(closed.status, DecisionStatus::Closed);

        let again = apply_outcome(closed, DecisionOutcome::Violated);
        assert_eq!(
            again.outcome,
            Some(DecisionOutcome::Fulfilled),
            "first outcome wins, second is a no-op"
        );
        assert_eq!(again.status, DecisionStatus::Closed);
    }

    /// Objective: Verify `apply_outcome` on an already-open decision still
    /// closes it, and an open decision starts with None outcome.
    /// Invariants: fresh decision is Open with None outcome.
    #[test]
    fn fresh_decision_is_open_with_no_outcome() {
        let fresh = sample_decision();
        assert_eq!(fresh.outcome, None);
        assert_eq!(fresh.status, DecisionStatus::Open);
    }
}
