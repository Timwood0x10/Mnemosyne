//! Agent fact channel — three-state attribution for AI conversation facts.
//!
//! The cognition compiler's existing user-fact path is strictly user-authored:
//! only `role = "user"` messages produce Preference/Goal/Emotion facts about
//! the User entity. That guarantees User cognition is never polluted by what
//! the assistant says (dev_guide "agent 不替用户表态").
//!
//! Real conversations also carry two other useful signals:
//!
//! - **Agent facts** — what the agent *did*. An assistant message that invokes
//!   a tool or reports a completed action becomes an `Event` fact attributed to
//!   the **Agent** entity (resolved via [`crate::fact_store::SqliteFactStore::resolve_agent`]),
//!   never to the User. Evidence points at the tool-call response fragment.
//! - **Derived facts** — what the agent *says the user thinks*. An assistant
//!   restatement like "you like Rust" or "你喜欢 Rust" becomes a
//!   Preference/Goal/Emotion fact attributed to the User entity BUT marked
//!   `attribution = "agent_derived"` with a confidence discount and DOUBLE
//!   evidence (the agent's restatement + the preceding user message). Consumers
//!   can weight these below direct user facts so they never override first-hand
//!   statements.
//!
//! ## Zero-pollution invariant
//!
//! [`ConversationFacts::user_facts`] is produced by the existing
//! `compile_user_facts` path and contains ONLY user-authored facts. Agent and
//! derived facts live in separate channels. The regression tests in
//! [`cognition_compiler`] and here lock that invariant.

use crate::cognition::{Fact, FactType};
use crate::types::Message;

/// Confidence multiplier applied to agent-derived restatements.
///
/// Direct user statements keep confidence 1.0; an agent restatement is never
/// allowed to outrank a first-hand statement of the same fact, so its
/// confidence is halved (external-knowledge-plan §C3).
pub const DERIVED_CONFIDENCE_DISCOUNT: f64 = 0.5;

/// Three-state attribution of facts extracted from one AI conversation.
///
/// All three channels are populated by [`extract_conversation_facts`]; the
/// caller persists each channel with the appropriate entity id and weighting.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConversationFacts {
    /// First-hand user cognition. ONLY from `role = "user"` messages.
    /// Attribute to the User entity. This channel is never polluted by agent
    /// or derived content (zero-pollution invariant).
    pub user_facts: Vec<Fact>,
    /// What the agent did (tool calls, completed actions). `Event` facts only,
    /// attributed to the **Agent** entity (never the User).
    pub agent_facts: Vec<Fact>,
    /// Agent restatements of user cognition. Attributed to the User entity but
    /// marked `attribution = "agent_derived"` with discounted confidence and
    /// double evidence. Consumers weight these below `user_facts`.
    pub derived_facts: Vec<Fact>,
}

impl ConversationFacts {
    /// Build an empty three-state container.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a container from pre-computed channels.
    #[must_use]
    pub fn from_channels(
        user_facts: Vec<Fact>,
        agent_facts: Vec<Fact>,
        derived_facts: Vec<Fact>,
    ) -> Self {
        Self {
            user_facts,
            agent_facts,
            derived_facts,
        }
    }

    /// Total number of facts across all three channels.
    #[must_use]
    pub fn total(&self) -> usize {
        self.user_facts.len() + self.agent_facts.len() + self.derived_facts.len()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Agent fact extraction (C2 + C5)
// ───────────────────────────────────────────────────────────────────────────

/// Completion-language markers indicating the agent reports a finished action.
///
/// Mirrors the decision-detection vocabulary in `conversation_compiler.rs` so
/// agent-action detection stays consistent with the legacy compiler.
///
/// Deliberately NO bare single character `已`: it appears inside common
/// fillers ("已经确认", "已完成一半") and made nearly any assistant message
/// count as a completed action. Only multi-char completion phrases survive.
const COMPLETION_MARKERS: &[&str] = &[
    "done",
    "implemented",
    "replaced",
    "completed",
    "fixed",
    "完成",
    "替换",
    "修复",
];

/// Extract `Event` facts describing what the agent did.
///
/// Two shapes produce an agent fact:
///
/// 1. An assistant message carrying a [`tool_invocation`](Message::tool_invocation)
///    — the agent executed a tool. Evidence includes the tool name, arguments,
///    and the matching tool-result message (found by `tool_call_id`) so the
///    fact is traceable to the exact tool-call response fragment (C5).
/// 2. An assistant message whose content contains a completion marker — the
///    agent reports a finished action. Evidence is the assistant message.
///
/// All agent facts are `FactType::Event` attributed to `agent_entity_id` and
/// tagged `attribution = "agent"` in the payload. They NEVER target the User
/// entity.
#[must_use]
pub fn agent_facts_from_messages(
    messages: &[Message],
    agent_entity_id: i64,
    logical_time: i32,
) -> Vec<Fact> {
    let mut facts = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        if !msg.is_assistant() {
            continue;
        }

        // Shape 1: tool invocation. The agent called a tool; pair it with the
        // tool-result message that follows within the same turn so evidence
        // points at the response fragment (see `find_tool_result` for the
        // positional, turn-bounded matching rationale).
        if let Some(inv) = &msg.tool_invocation {
            let tool_result = find_tool_result(messages, idx);
            let status = tool_result
                .map(|r| {
                    let lower = r.content.to_lowercase();
                    if lower.contains("error") || lower.contains("failed") {
                        "error"
                    } else {
                        "ok"
                    }
                })
                .unwrap_or("timeout");
            let mut payload = serde_json::json!({
                "attribution": "agent",
                "action": "tool_call",
                "tool": inv.name,
                "arguments": inv.arguments,
                "status": status,
            });
            if let Some(result) = tool_result {
                payload["tool_response"] = serde_json::Value::String(result.content.clone());
            }
            facts.push(Fact {
                id: None,
                entity_id: agent_entity_id,
                fact_type: FactType::Event,
                time: logical_time,
                payload,
                evidence_id: None,
                created_at: i64::from(logical_time),
            });
            continue;
        }

        // Shape 2: completion language. The agent reports a finished action.
        let lower = msg.content.to_lowercase();
        if COMPLETION_MARKERS
            .iter()
            .any(|m| lower.contains(m) || msg.content.contains(m))
        {
            facts.push(Fact {
                id: None,
                entity_id: agent_entity_id,
                fact_type: FactType::Event,
                time: logical_time,
                payload: serde_json::json!({
                    "attribution": "agent",
                    "action": "completed",
                    "content": msg.content,
                }),
                evidence_id: None,
                created_at: i64::from(logical_time),
            });
        }
    }
    facts
}

/// Find the tool-result message that answers the tool invocation at
/// `assistant_idx`.
///
/// A tool result is any message carrying a [`tool_call_id`](Message::tool_call_id)
/// (the framework routes tool outputs back with that id). The match is
/// **positional and turn-bounded**: the first `tool_call_id`-bearing message
/// after `assistant_idx` AND before the next assistant message is returned.
///
/// This deliberately avoids substring-matching the result content against the
/// tool name: a short tool name (e.g. `cat`) would otherwise false-positive on
/// unrelated content (e.g. `category`), and in multi-tool turns the wrong
/// result could be paired with the invocation. Exact `tool_call_id` matching
/// from the assistant side is not possible today because [`crate::types::ToolInvocation`]
/// carries no call id and the assistant [`Message`] leaves `tool_call_id` as
/// `None`; positional turn-bounded matching is the most precise heuristic
/// available without extending the data model.
///
/// Returns `None` when no tool-result message follows within the same turn
/// (the caller then treats the invocation as `status = "timeout"`).
fn find_tool_result(messages: &[Message], assistant_idx: usize) -> Option<&Message> {
    messages
        .iter()
        .skip(assistant_idx + 1)
        // Stop at the next assistant message: tool results belong to the
        // current turn, and anything after the next assistant turn is a
        // different invocation's response.
        .take_while(|m| !m.is_assistant())
        .find(|m| m.tool_call_id.is_some())
}

// ───────────────────────────────────────────────────────────────────────────
// Derived fact extraction (C3)
// ───────────────────────────────────────────────────────────────────────────

/// One restatement pattern: when the assistant says `marker`, the agent is
/// asserting the user has `fact_type` cognition about the restated content.
struct RestatementPattern {
    marker: &'static str,
    fact_type: FactType,
}

/// Bilingual restatement markers. Each marker is a phrase an assistant uses to
/// restate user cognition ("you like …", "你喜欢 …", "the user wants …").
const RESTATEMENT_PATTERNS: &[RestatementPattern] = &[
    RestatementPattern {
        marker: "你喜欢",
        fact_type: FactType::Preference,
    },
    RestatementPattern {
        marker: "你偏好",
        fact_type: FactType::Preference,
    },
    RestatementPattern {
        marker: "你想要",
        fact_type: FactType::Goal,
    },
    RestatementPattern {
        marker: "你计划",
        fact_type: FactType::Goal,
    },
    RestatementPattern {
        marker: "你打算",
        fact_type: FactType::Goal,
    },
    RestatementPattern {
        marker: "你感到",
        fact_type: FactType::Emotion,
    },
    RestatementPattern {
        marker: "你觉得",
        fact_type: FactType::Emotion,
    },
    RestatementPattern {
        marker: "you like",
        fact_type: FactType::Preference,
    },
    RestatementPattern {
        marker: "you prefer",
        fact_type: FactType::Preference,
    },
    RestatementPattern {
        marker: "you want",
        fact_type: FactType::Goal,
    },
    RestatementPattern {
        marker: "you plan",
        fact_type: FactType::Goal,
    },
    RestatementPattern {
        marker: "you feel",
        fact_type: FactType::Emotion,
    },
    RestatementPattern {
        marker: "the user likes",
        fact_type: FactType::Preference,
    },
    RestatementPattern {
        marker: "the user prefers",
        fact_type: FactType::Preference,
    },
    RestatementPattern {
        marker: "the user wants",
        fact_type: FactType::Goal,
    },
];

/// Extract agent-derived restatements of user cognition.
///
/// Each assistant message is scanned for [`RESTATEMENT_PATTERNS`]. A match
/// produces a fact attributed to `user_entity_id` (the claim is ABOUT the
/// user) with:
///
/// - `attribution = "agent_derived"` — so consumers can filter/weight it.
/// - `confidence = DERIVED_CONFIDENCE_DISCOUNT` — never outranks a direct
///   user statement of the same fact.
/// - **Double evidence**: the agent's restatement AND the nearest preceding
///   user message (the statement the agent is presumably restating).
///
/// These facts live in a SEPARATE channel from [`agent_facts_from_messages`];
/// they never enter the `user_facts` channel (zero-pollution invariant).
#[must_use]
pub fn derived_facts_from_messages(
    messages: &[Message],
    user_entity_id: i64,
    logical_time: i32,
) -> Vec<Fact> {
    let mut facts = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        if !msg.is_assistant() {
            continue;
        }
        let lower = msg.content.to_lowercase();
        for pattern in RESTATEMENT_PATTERNS {
            if !lower.contains(pattern.marker) && !msg.content.contains(pattern.marker) {
                continue;
            }
            let preceding_user = nearest_preceding_user(messages, idx);
            let mut evidence = serde_json::json!({
                "agent_restatement": msg.content,
            });
            if let Some(user_msg) = preceding_user {
                evidence["user_message"] = serde_json::Value::String(user_msg.content.clone());
            }
            facts.push(Fact {
                id: None,
                entity_id: user_entity_id,
                fact_type: pattern.fact_type,
                time: logical_time,
                payload: serde_json::json!({
                    "attribution": "agent_derived",
                    "content": msg.content,
                    "confidence": DERIVED_CONFIDENCE_DISCOUNT,
                    "evidence": evidence,
                }),
                evidence_id: None,
                created_at: i64::from(logical_time),
            });
        }
    }
    facts
}

/// Return the nearest `role = "user"` message at an index strictly before `idx`.
fn nearest_preceding_user(messages: &[Message], idx: usize) -> Option<&Message> {
    messages.iter().take(idx).rev().find(|m| m.is_user())
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::FactType;
    use crate::types::{Message, ToolInvocation};

    /// Objective: Verify agent_facts_from_messages produces an Event fact for a
    /// tool-invoking assistant message, attributed to the Agent entity (not User).
    /// Invariants: One Event fact; entity_id is the agent; payload attribution
    /// is "agent"; tool name + arguments preserved.
    #[test]
    fn agent_facts_capture_tool_invocation() {
        let messages = vec![
            Message::new("user", "search for rust async docs"),
            Message {
                role: "assistant".into(),
                content: "calling search".into(),
                tool_call_id: None,
                tool_invocation: Some(ToolInvocation {
                    name: "web_search".into(),
                    arguments: r#"{"q":"rust async"}"#.into(),
                }),
                turn_id: None,
            },
            Message {
                role: "tool".into(),
                content: "web_search returned 3 results".into(),
                tool_call_id: Some("call-1".into()),
                tool_invocation: None,
                turn_id: None,
            },
        ];
        let facts = agent_facts_from_messages(&messages, 77, 2_026_073_000);
        assert_eq!(facts.len(), 1, "one tool invocation → one agent Event fact");
        let fact = &facts[0];
        assert_eq!(fact.entity_id, 77, "attributed to the Agent entity");
        assert_eq!(fact.fact_type, FactType::Event, "agent actions are Events");
        assert_eq!(
            fact.payload["attribution"], "agent",
            "payload marks the agent channel"
        );
        assert_eq!(fact.payload["tool"], "web_search", "tool name preserved");
        assert_eq!(
            fact.payload["status"], "ok",
            "status derived from tool result content"
        );
        assert!(
            fact.payload["tool_response"]
                .as_str()
                .unwrap_or("")
                .contains("3 results"),
            "evidence points at the tool-call response fragment (C5)"
        );
    }

    /// Objective: Verify a completion-language assistant message (no tool
    /// invocation) still produces an agent Event fact.
    /// Invariants: "Done, implemented X" → one Event fact attributed to agent.
    #[test]
    fn agent_facts_capture_completion_language() {
        let messages = vec![
            Message::new("user", "please fix the parser"),
            Message::new("assistant", "Done, I have implemented the parser fix."),
        ];
        let facts = agent_facts_from_messages(&messages, 9, 100);
        assert_eq!(facts.len(), 1, "completion language → one agent fact");
        assert_eq!(facts[0].entity_id, 9);
        assert_eq!(facts[0].fact_type, FactType::Event);
        assert_eq!(facts[0].payload["action"], "completed");
    }

    /// Objective: Verify pure user messages produce NO agent facts (agent
    /// channel only fires on assistant content).
    /// Invariants: A user-only conversation yields zero agent facts.
    #[test]
    fn agent_facts_ignore_user_messages() {
        let messages = vec![
            Message::new("user", "I like Rust"),
            Message::new("user", "I want to learn async"),
        ];
        let facts = agent_facts_from_messages(&messages, 5, 1);
        assert!(facts.is_empty(), "user messages never produce agent facts");
    }

    /// Objective: Verify derived_facts_from_messages extracts an agent
    /// restatement with the correct fact type, discounted confidence, and
    /// double evidence.
    /// Invariants: "you like Rust" → one Preference fact attributed to User;
    /// attribution "agent_derived"; confidence 0.5; evidence has both
    /// agent_restatement and the preceding user_message.
    #[test]
    fn derived_facts_extract_restatement_with_double_evidence() {
        let messages = vec![
            Message::new("user", "I enjoy writing Rust on weekends"),
            Message::new(
                "assistant",
                "Got it — you like Rust, so I'll suggest async tips.",
            ),
        ];
        let facts = derived_facts_from_messages(&messages, 42, 2_026_073_000);
        assert_eq!(facts.len(), 1, "one restatement → one derived fact");
        let fact = &facts[0];
        assert_eq!(fact.entity_id, 42, "derived fact is ABOUT the User entity");
        assert_eq!(fact.fact_type, FactType::Preference);
        assert_eq!(
            fact.payload["attribution"], "agent_derived",
            "marked as agent-derived so consumers can downweight it"
        );
        let confidence = fact.payload["confidence"].as_f64().unwrap_or(1.0);
        assert!(
            (confidence - DERIVED_CONFIDENCE_DISCOUNT).abs() < f64::EPSILON,
            "confidence must be discounted to {DERIVED_CONFIDENCE_DISCOUNT}, got {confidence}"
        );
        let evidence = &fact.payload["evidence"];
        assert!(
            evidence["agent_restatement"]
                .as_str()
                .unwrap_or("")
                .contains("you like Rust"),
            "first evidence is the agent's restatement"
        );
        assert!(
            evidence["user_message"]
                .as_str()
                .unwrap_or("")
                .contains("weekends"),
            "second evidence is the preceding user message (double evidence)"
        );
    }

    /// Objective: Verify derived facts cover Chinese restatement markers and
    /// map to the correct fact type.
    /// Invariants: "你喜欢 Go" → Preference; "你计划" → Goal.
    #[test]
    fn derived_facts_handle_chinese_markers() {
        let messages = vec![
            Message::new("user", "我对 Go 很感兴趣"),
            Message::new("assistant", "看来你喜欢 Go，我会推荐相关资料。"),
        ];
        let facts = derived_facts_from_messages(&messages, 3, 1);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].fact_type, FactType::Preference);

        let messages_goal = vec![
            Message::new("user", "我想学新东西"),
            Message::new("assistant", "你计划学 Rust，对吗？"),
        ];
        let facts_goal = derived_facts_from_messages(&messages_goal, 3, 1);
        assert_eq!(facts_goal.len(), 1);
        assert_eq!(facts_goal[0].fact_type, FactType::Goal);
    }

    /// Objective: Verify derived facts are produced even when there is NO
    /// preceding user message (the agent restates unprompted) — evidence then
    /// carries only the agent_restatement.
    /// Invariants: One derived fact; evidence has agent_restatement but no
    /// user_message key.
    #[test]
    fn derived_facts_without_preceding_user_message() {
        let messages = vec![Message::new(
            "assistant",
            "Based on context, you prefer tab indentation.",
        )];
        let facts = derived_facts_from_messages(&messages, 7, 1);
        assert_eq!(facts.len(), 1, "derived fact still extracted");
        let evidence = &facts[0].payload["evidence"];
        assert!(
            evidence["agent_restatement"].is_string(),
            "agent restatement is always present"
        );
        assert!(
            evidence.get("user_message").is_none(),
            "no user_message when nothing precedes"
        );
    }

    /// Objective: Verify the zero-pollution invariant — agent and derived
    /// channels NEVER appear in the user_facts channel of ConversationFacts.
    /// Invariants: user_facts comes ONLY from the user path; agent/derived
    /// content stays in its own channel.
    #[test]
    fn conversation_facts_keep_channels_separated() {
        // user_facts channel is supplied by the existing user path (simulated
        // here with a hand-built Goal fact from a user message); agent and
        // derived channels are extracted from the assistant messages.
        let messages = vec![
            Message::new("user", "I want to learn Rust"),
            Message::new("assistant", "you want to learn Rust, great!"),
            Message::new("assistant", "Done, I set up a Rust learning plan."),
        ];
        let user_facts = crate::conversation_compiler::compile_user_facts(&messages, 42, 1);
        let agent_facts = agent_facts_from_messages(&messages, 99, 1);
        let derived_facts = derived_facts_from_messages(&messages, 42, 1);

        let conv = ConversationFacts::from_channels(user_facts, agent_facts, derived_facts);

        // user_facts: only from user messages — no "agent"/"agent_derived".
        assert!(
            !conv.user_facts.is_empty(),
            "user path still produces first-hand facts"
        );
        assert!(
            conv.user_facts
                .iter()
                .all(|f| { f.payload.get("attribution").is_none() }),
            "user_facts must carry NO attribution marker (zero-pollution)"
        );
        // agent_facts: all Event, all attributed to the Agent entity.
        assert!(
            !conv.agent_facts.is_empty(),
            "agent completion captured in its own channel"
        );
        assert!(
            conv.agent_facts
                .iter()
                .all(|f| f.entity_id == 99 && f.fact_type == FactType::Event),
            "agent channel targets the Agent entity with Event facts only"
        );
        // derived_facts: all agent_derived, attributed to User but discounted.
        assert!(
            !conv.derived_facts.is_empty(),
            "derived restatement captured in its own channel"
        );
        assert!(
            conv.derived_facts.iter().all(|f| {
                f.payload.get("attribution")
                    == Some(&serde_json::Value::String("agent_derived".into()))
            }),
            "derived channel is marked agent_derived"
        );
        // No fact id appears in more than one channel (channels are disjoint).
        assert_eq!(
            conv.total(),
            conv.user_facts.len() + conv.agent_facts.len() + conv.derived_facts.len(),
            "total() sums the three disjoint channels"
        );
    }

    /// Objective: Verify a tool invocation whose result reports an error is
    /// tagged status=error (so downstream can filter failed agent actions).
    /// Invariants: tool result containing "error" → status "error".
    #[test]
    fn agent_facts_tag_error_status_from_tool_result() {
        let messages = vec![
            Message {
                role: "assistant".into(),
                content: "running build".into(),
                tool_call_id: None,
                tool_invocation: Some(ToolInvocation {
                    name: "build".into(),
                    arguments: "{}".into(),
                }),
                turn_id: None,
            },
            Message {
                role: "tool".into(),
                content: "build failed with error E0308".into(),
                tool_call_id: Some("c1".into()),
                tool_invocation: None,
                turn_id: None,
            },
        ];
        let facts = agent_facts_from_messages(&messages, 1, 1);
        assert_eq!(facts.len(), 1);
        assert_eq!(
            facts[0].payload["status"], "error",
            "failed tool → error status"
        );
    }

    /// Objective: Verify `find_tool_result` pairs an invocation with the
    /// IMMEDIATE following tool-result even when the result content does NOT
    /// echo the tool name (the old substring match would have missed it), and
    /// that a tool result in a LATER turn is never matched across an
    /// intervening assistant message (turn-bounded matching).
    /// Invariants: invocation of "cat" pairs with the next tool-result message
    /// (status "ok") whose content lacks "cat"; a second turn's result is NOT
    /// matched when the first turn has no result (status "timeout").
    #[test]
    fn find_tool_result_is_positional_and_turn_bounded() {
        // Turn 1: "cat" invocation with its result (result content has no
        // "cat" substring — the old name-matching path would skip it).
        let messages = vec![
            Message {
                role: "assistant".into(),
                content: "running lookup".into(),
                tool_call_id: None,
                tool_invocation: Some(ToolInvocation {
                    name: "cat".into(),
                    arguments: "{}".into(),
                }),
                turn_id: None,
            },
            Message {
                role: "tool".into(),
                content: "the category is animals".into(),
                tool_call_id: Some("c1".into()),
                tool_invocation: None,
                turn_id: None,
            },
        ];
        // Positional match returns the immediate result despite "cat" being a
        // substring of "category" (no false-positive from the other direction,
        // and no miss because the result omits the tool name).
        let result = find_tool_result(&messages, 0);
        assert_eq!(
            result.map(|m| m.content.as_str()),
            Some("the category is animals"),
            "immediate tool-result is paired regardless of name echo"
        );
        let facts = agent_facts_from_messages(&messages, 1, 1);
        assert_eq!(facts[0].payload["status"], "ok", "immediate result → ok");

        // Turn-bounded: a later assistant turn separates the invocation from a
        // result, so the invocation in turn 1 has NO result in its own turn.
        let multi_turn = vec![
            Message {
                role: "assistant".into(),
                content: "calling cat".into(),
                tool_call_id: None,
                tool_invocation: Some(ToolInvocation {
                    name: "cat".into(),
                    arguments: "{}".into(),
                }),
                turn_id: None,
            },
            // Next assistant turn begins — turn 1's window ends here.
            Message::new("assistant", "moving on, let me build now."),
            Message {
                role: "tool".into(),
                content: "build ok".into(),
                tool_call_id: Some("c2".into()),
                tool_invocation: None,
                turn_id: None,
            },
        ];
        assert!(
            find_tool_result(&multi_turn, 0).is_none(),
            "a tool result past the next assistant message is out of turn"
        );
        let facts = agent_facts_from_messages(&multi_turn, 1, 1);
        // The cat invocation (turn 1) has no in-turn result → status "timeout".
        assert_eq!(
            facts[0].payload["status"], "timeout",
            "no in-turn result → timeout (not a cross-turn mis-pairing)"
        );
    }

    /// Objective: Verify ConversationFacts::new and total on an empty container.
    /// Invariants: new() yields three empty channels; total() == 0.
    #[test]
    fn conversation_facts_new_is_empty() {
        let conv = ConversationFacts::new();
        assert!(conv.user_facts.is_empty());
        assert!(conv.agent_facts.is_empty());
        assert!(conv.derived_facts.is_empty());
        assert_eq!(conv.total(), 0);
    }
}
