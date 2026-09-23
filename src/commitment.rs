//! Deterministic commitment extraction — the Decision write path.
//!
//! A [`Decision`] records what a speaker committed to, why, and what happened
//! afterwards. The decision layer deliberately exposes only two read-only MCP
//! tools (`decision_trace`, `decision_search`), so decisions are produced by
//! **compilation** — exactly like every other fact in this engine — rather than
//! by a tool call.
//!
//! Extraction is rule-driven and LLM-free: an utterance becomes a decision only
//! when it carries an explicit commitment marker. `because` is filled with the
//! compiled facts whose text backs the commitment: supporting evidence, never
//! causality.
//!
//! Marker matching is token-aware (an ASCII marker must stand on its own, so
//! "compromise" is not a promise) and negation-aware (a negated utterance is
//! not a commitment, so "I will not help you" is never recorded as one).

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

/// Negation cues that cancel a commitment.
///
/// A decision layer is an audit surface: recording the *opposite* of what was
/// said ("I will not help you" as a commitment) is worse than missing one, so a
/// cue anywhere before the marker inside its clause, or immediately after it,
/// suppresses the decision.
const NEGATION_CUES: &[&str] = &[
    "不", "没", "别", "未", "无", "非", "拒绝", "not", "never", "no", "won't", "cannot", "can't",
];

/// Characters that end a clause. A negation before one of these belongs to a
/// different statement and must not cancel this commitment.
const CLAUSE_BREAKS: &[char] = &[
    '，', '。', '！', '？', '；', '、', '：', ',', '.', '!', '?', ';', ':',
];

/// How many characters after the marker still count as "immediately negated"
/// ("保证不去", "I will not …").
const NEGATION_WINDOW_CHARS: usize = 3;

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
///
/// The **earliest** marker in the text decides the verb, and a negated
/// commitment is rejected outright (see [`negated_commitment`]).
fn commitment_from_message(message: &Message, subject: i64, made_at: i32) -> Option<Decision> {
    let lowered = message.content.to_lowercase();
    let (verb, at, marker_len) = COMMITMENT_MARKERS
        .iter()
        .filter_map(|(marker, verb)| {
            let needle = marker.to_lowercase();
            find_marker(&lowered, &needle).map(|at| (*verb, at, needle.len()))
        })
        .min_by_key(|(_, at, _)| *at)?;
    if negated_commitment(&lowered, at, marker_len) {
        return None;
    }
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

/// Find `marker` in `haystack`, requiring an ASCII marker to *start* on a word
/// boundary so that `promise` never matches inside `compromise`.
///
/// Only the start is guarded: inflections such as "promises"/"promised" are
/// still commitments, while "compromise" is not. CJK markers have no word
/// boundaries and are matched literally.
fn find_marker(haystack: &str, marker: &str) -> Option<usize> {
    if !marker.is_ascii() {
        return haystack.find(marker);
    }
    haystack
        .match_indices(marker)
        .find(|(at, _)| is_boundary_char(haystack[..*at].chars().next_back()))
        .map(|(at, _)| at)
}

/// A character that may sit next to an ASCII marker (anything but a letter or
/// digit, or the edge of the text).
fn is_boundary_char(candidate: Option<char>) -> bool {
    match candidate {
        Some(value) => !value.is_alphanumeric(),
        None => true,
    }
}

/// True when the commitment at `at` is actually negated.
///
/// Two signals cancel it:
///
/// - a negation cue in the same clause **before** the marker ("我不保证…"),
/// - a negation cue within [`NEGATION_WINDOW_CHARS`] characters **after** it
///   ("保证不去", "I will not …").
///
/// Deliberately conservative: a cue far behind the marker still counts as a
/// commitment ("我答应你明天不迟到"), because rejecting every message that
/// merely mentions a negation would swallow most real promises.
fn negated_commitment(lowered: &str, at: usize, marker_len: usize) -> bool {
    let prefix = &lowered[..at];
    let clause_start = prefix
        .rfind(|c: char| CLAUSE_BREAKS.contains(&c))
        .map_or(0, |index| index + 1);
    if contains_negation_cue(&prefix[clause_start..]) {
        return true;
    }
    let window: String = lowered[at + marker_len..]
        .chars()
        .take(NEGATION_WINDOW_CHARS)
        .collect();
    contains_negation_cue(&window)
}

/// True when `text` carries any [`NEGATION_CUES`] entry.
fn contains_negation_cue(text: &str) -> bool {
    NEGATION_CUES.iter().any(|cue| text.contains(cue))
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

    /// Objective: Verify an ASCII marker must start on a word boundary. Plain
    /// substring matching turned "we reached a compromise" into a promise
    /// ("compromise" contains "promise"), fabricating a decision nobody made.
    /// Invariants: a marker embedded in a longer word yields no decision, while
    /// an inflected genuine commitment still does.
    #[test]
    fn ascii_markers_must_start_on_a_word_boundary() {
        let embedded = vec![message("user", "we reached a compromise")];
        assert!(
            commitments_from_messages(&embedded, "user", 7, 2026).is_empty(),
            "a marker inside another word must not create a decision"
        );

        let inflected = vec![message("user", "he promises to be here")];
        let decisions = commitments_from_messages(&inflected, "user", 7, 2026);
        assert_eq!(
            decisions.len(),
            1,
            "an inflected promise is still a commitment"
        );
        assert_eq!(decisions[0].verb, "promise");
    }

    /// Objective: Verify a negated utterance is never recorded as a commitment.
    /// Recording its opposite is the worst failure mode for an audit layer.
    /// Invariants: an adjacent or preceding negation cue suppresses the
    /// decision for both Chinese and English markers.
    #[test]
    fn negated_utterances_are_not_commitments() {
        for content in [
            "我保证不去",
            "我不保证能到",
            "I will not help you",
            "I promise nothing",
            "我答应你，不，我拒绝",
        ] {
            let decisions = commitments_from_messages(&[message("user", content)], "user", 7, 2026);
            assert!(
                decisions.is_empty(),
                "`{content}` must not be recorded as a commitment, got {decisions:?}"
            );
        }
    }

    /// Objective: Verify the negation guard stays conservative: a cue that
    /// belongs to a later part of the promise must not swallow it.
    /// Invariants: "我答应你明天不迟到" is still one promise.
    #[test]
    fn distant_negation_still_counts_as_a_commitment() {
        let decisions =
            commitments_from_messages(&[message("user", "我答应你明天不迟到")], "user", 7, 2026);
        assert_eq!(
            decisions.len(),
            1,
            "a cue behind the marker describes the promise, it does not negate it"
        );
        assert_eq!(decisions[0].verb, "promise");
    }

    /// Objective: Verify the verb comes from the earliest marker in the text, so
    /// a later marker cannot relabel an earlier promise.
    /// Invariants: "我答应你，我保证会做到" is a promise, not a commit.
    #[test]
    fn the_earliest_marker_decides_the_verb() {
        let decisions = commitments_from_messages(
            &[message("user", "我答应你，我保证会做到")],
            "user",
            7,
            2026,
        );
        assert_eq!(decisions.len(), 1);
        assert_eq!(
            decisions[0].verb, "promise",
            "the first commitment in the text decides the verb"
        );
    }
}
