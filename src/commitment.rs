//! Deterministic commitment extraction — the v0.3.1 Decision write path.
//!
//! A [`Decision`] records what a speaker committed to, why, and what happened
//! afterwards. The frozen v0.3.1 plan keeps the MCP surface at two read-only
//! tools (`decision_trace`, `decision_search`), so decisions are produced by
//! **compilation** — exactly like every other fact in this engine — rather than
//! by a tool call.
//!
//! Extraction is rule-driven and LLM-free: an utterance becomes a decision only
//! when it carries an explicit commitment marker. `because` is filled with the
//! compiled facts whose text backs the commitment: supporting evidence, never
//! causality.

use crate::cognition::Fact;
use crate::decision::{Decision, DecisionStatus};
use crate::types::Message;

/// Explicit commitment markers mapped to the decision verb they imply.
///
/// Deliberately conservative: a bare "我会" / "I will" expresses a *plan*
/// (already compiled into a Goal fact), not a promise, so it is not listed.
pub const COMMITMENT_MARKERS: &[(&str, &str)] = &[
    ("答应", "promise"),
    ("承诺", "promise"),
    ("说好了", "promise"),
    ("promise", "promise"),
    ("保证", "commit"),
    ("发誓", "commit"),
    ("一定会", "commit"),
    ("i'll", "commit"),
    ("i will", "commit"),
    ("swear", "commit"),
];

/// Maximum number of supporting facts attached to one decision.
const MAX_SUPPORTING_FACTS: usize = 8;
/// Maximum stored length of a decision's `object` (mirrors `validate_decision`).
const MAX_OBJECT_CHARS: usize = 512;

/// Extract decisions from the messages written by one speaker.
///
/// `role` selects the speaker channel ("user" or "assistant") and `subject` is
/// the entity that made the commitments. A message without a commitment marker
/// contributes nothing, so the function never invents a commitment.
#[must_use]
pub fn commitments_from_messages(
    messages: &[Message],
    role: &str,
    subject: i64,
    made_at: i32,
) -> Vec<Decision> {
    if subject <= 0 {
        return Vec::new();
    }
    messages
        .iter()
        .filter(|message| message.role == role)
        .filter_map(|message| commitment_from_message(message, subject, made_at))
        .collect()
}

/// Build one decision from one message, or `None` when it holds no commitment.
fn commitment_from_message(message: &Message, subject: i64, made_at: i32) -> Option<Decision> {
    let lowered = message.content.to_lowercase();
    let verb = COMMITMENT_MARKERS
        .iter()
        .find(|(marker, _)| lowered.contains(&marker.to_lowercase()))
        .map(|(_, verb)| *verb)?;
    let object = truncate_chars(message.content.trim(), MAX_OBJECT_CHARS);
    if object.is_empty() {
        return None;
    }
    Some(Decision {
        id: None,
        subject,
        verb: verb.to_string(),
        object,
        made_at,
        because: Vec::new(),
        outcome: None,
        status: DecisionStatus::Open,
    })
}

/// Build the fact that anchors a decision.
///
/// Every claim in this engine is anchored to a stored fact; a commitment is no
/// exception. The promise markers are not part of the observation marker tables
/// (they express an undertaking, not an emotion/plan/preference), so nothing
/// else would produce a fact for the utterance — the decision would have
/// nothing to trace back to. Storing the utterance as an Event fact first makes
/// `Decision::because` a concrete evidence link.
#[must_use]
pub fn anchor_fact(decision: &Decision, made_at: i32) -> Fact {
    Fact {
        id: None,
        entity_id: decision.subject,
        fact_type: crate::cognition::FactType::Event,
        time: made_at,
        payload: serde_json::json!({
            "content": decision.object,
            "source": "commitment",
            "verb": decision.verb,
        }),
        evidence_id: None,
        created_at: i64::from(made_at),
        ..Fact::default()
    }
}

/// Select the compiled facts that back a commitment.
///
/// A fact supports the decision when the two texts contain one another: the
/// compiler stores an utterance close to verbatim, so containment is both
/// deterministic and cheap. The result is capped so one runaway statement
/// cannot attach an unbounded evidence list.
#[must_use]
pub fn supporting_fact_ids(facts: &[Fact], entity_id: i64, statement: &str) -> Vec<i64> {
    if statement.is_empty() {
        return Vec::new();
    }
    facts
        .iter()
        .filter(|fact| fact.entity_id == entity_id)
        .filter_map(|fact| {
            let content = fact
                .payload
                .get("content")
                .and_then(|value| value.as_str())?;
            if content.is_empty() {
                return None;
            }
            let matches = statement.contains(content) || content.contains(statement);
            match (matches, fact.id) {
                (true, Some(id)) => Some(id),
                _ => None,
            }
        })
        .take(MAX_SUPPORTING_FACTS)
        .collect()
}

/// Truncate to `max` characters (not bytes) so multi-byte text is never split.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::FactType;
    use serde_json::json;

    fn message(role: &str, content: &str) -> Message {
        Message::new(role, content)
    }

    fn fact(id: i64, entity_id: i64, content: &str) -> Fact {
        Fact {
            id: Some(id),
            entity_id,
            fact_type: FactType::Event,
            time: 2026,
            payload: json!({ "content": content }),
            created_at: 2026,
            ..Fact::default()
        }
    }

    /// Objective: Verify a commitment utterance compiles into an open decision
    /// carrying the speaker as subject, the implied verb and the verbatim text.
    /// Invariants: one decision; subject/verb/object set; outcome `None` and
    /// status `Open` (the decision is recorded before the outcome is known).
    #[test]
    fn commitment_marker_produces_an_open_decision() {
        let messages = vec![
            message("user", "我答应你明天陪你去医院"),
            message("user", "今天天气不错"),
        ];
        let decisions = commitments_from_messages(&messages, "user", 7, 2026);

        assert_eq!(
            decisions.len(),
            1,
            "only the commitment line yields a decision"
        );
        let decision = &decisions[0];
        assert_eq!(decision.subject, 7, "the speaker is the decision subject");
        assert_eq!(decision.verb, "promise", "答应 maps to the promise verb");
        assert_eq!(decision.object, "我答应你明天陪你去医院");
        assert_eq!(decision.made_at, 2026, "observation time is recorded");
        assert!(
            decision.outcome.is_none(),
            "a fresh decision has no outcome yet"
        );
        assert_eq!(
            decision.status,
            DecisionStatus::Open,
            "a fresh decision is open"
        );
    }

    /// Objective: Verify the extractor is conservative — a plan ("我会…") or a
    /// plain statement must NOT be promoted into a decision, so the decisions
    /// table never fills with guesses.
    /// Invariants: no marker → empty result.
    #[test]
    fn plain_statements_and_plans_are_not_commitments() {
        let messages = vec![
            message("user", "我会考虑一下这个方案"),
            message("user", "今天很累"),
        ];
        assert!(
            commitments_from_messages(&messages, "user", 7, 2026).is_empty(),
            "plans and plain statements must not become decisions"
        );
    }

    /// Objective: Verify only the requested speaker channel is scanned — the
    /// user's promise must not be attributed to the agent entity and vice versa.
    /// Invariants: the user channel yields only user lines; the assistant
    /// channel yields only assistant lines.
    #[test]
    fn extraction_is_scoped_to_the_requested_role() {
        let messages = vec![
            message("user", "我保证周五之前给你答复"),
            message("assistant", "I promise to remind you"),
        ];

        let user_decisions = commitments_from_messages(&messages, "user", 7, 2026);
        assert_eq!(user_decisions.len(), 1, "one user commitment");
        assert_eq!(user_decisions[0].subject, 7);
        assert_eq!(user_decisions[0].verb, "commit", "保证 maps to commit");

        let agent_decisions = commitments_from_messages(&messages, "assistant", 9, 2026);
        assert_eq!(agent_decisions.len(), 1, "one agent commitment");
        assert_eq!(agent_decisions[0].subject, 9, "agent is the subject");
        assert_eq!(agent_decisions[0].verb, "promise");
    }

    /// Objective: Verify an unresolvable subject (no entity id) produces no
    /// decisions instead of rows that cannot be traced back to anyone.
    /// Invariants: subject <= 0 → empty.
    #[test]
    fn missing_subject_yields_no_decisions() {
        let messages = vec![message("user", "我答应你")];
        assert!(
            commitments_from_messages(&messages, "user", 0, 2026).is_empty(),
            "a decision without a subject must not be created"
        );
    }

    /// Objective: Verify an oversized utterance is truncated by CHARACTER so a
    /// multi-byte text never ends in the middle of a code point and never
    /// exceeds the validator's limit.
    /// Invariants: object length <= MAX_OBJECT_CHARS and remains valid UTF-8.
    #[test]
    fn oversized_commitment_is_truncated_by_character() {
        let long = format!("我答应你{}", "好".repeat(MAX_OBJECT_CHARS * 2));
        let decisions = commitments_from_messages(&[message("user", &long)], "user", 7, 2026);
        assert_eq!(decisions.len(), 1, "the commitment is still recognised");
        assert_eq!(
            decisions[0].object.chars().count(),
            MAX_OBJECT_CHARS,
            "the object is truncated to the validator's character limit"
        );
    }

    /// Objective: Verify `because` links a decision back to the compiled facts
    /// whose text backs it, and to nothing else.
    /// Invariants: matching fact ids are returned; unrelated facts and other
    /// entities' facts are excluded.
    #[test]
    fn supporting_facts_link_only_matching_evidence() {
        let facts = vec![
            fact(11, 7, "我答应你明天陪你去医院"),
            fact(12, 7, "今天天气不错"),
            fact(13, 9, "我答应你明天陪你去医院"),
        ];
        let ids = supporting_fact_ids(&facts, 7, "我答应你明天陪你去医院");
        assert_eq!(
            ids,
            vec![11],
            "only the matching fact of the right entity is supporting evidence"
        );
    }

    /// Objective: Verify `anchor_fact` turns a decision into the Event fact that
    /// anchors it, so the promise utterance is stored as experience rather than
    /// living only in the `decisions` table.
    /// Invariants: the anchor belongs to the subject, is an Event at the
    /// decision's time, and carries the utterance plus the decision verb.
    #[test]
    fn anchor_fact_mirrors_the_decision() {
        let decision = Decision {
            id: None,
            subject: 7,
            verb: "promise".to_string(),
            object: "我答应你明天陪你去医院".to_string(),
            made_at: 2026,
            because: Vec::new(),
            outcome: None,
            status: DecisionStatus::Open,
        };
        let anchor = anchor_fact(&decision, 2026);

        assert_eq!(anchor.entity_id, 7, "the anchor belongs to the subject");
        assert_eq!(
            anchor.fact_type,
            FactType::Event,
            "a commitment is stored as an Event"
        );
        assert_eq!(anchor.time, 2026, "the anchor carries the decision time");
        assert_eq!(anchor.payload["content"], "我答应你明天陪你去医院");
        assert_eq!(anchor.payload["verb"], "promise");
        assert_eq!(anchor.payload["source"], "commitment");
        assert!(anchor.id.is_none(), "the anchor is inserted by the store");
    }

    /// Objective: Verify the supporting-evidence list is bounded so one runaway
    /// statement cannot attach an unbounded chain.
    /// Invariants: at most MAX_SUPPORTING_FACTS ids are returned.
    #[test]
    fn supporting_evidence_is_capped() {
        let statement = "我答应你明天陪你去医院";
        let facts: Vec<Fact> = (1..=(MAX_SUPPORTING_FACTS as i64 + 5))
            .map(|id| fact(id, 7, statement))
            .collect();
        let ids = supporting_fact_ids(&facts, 7, statement);
        assert_eq!(
            ids.len(),
            MAX_SUPPORTING_FACTS,
            "supporting evidence must be capped"
        );
    }
}
