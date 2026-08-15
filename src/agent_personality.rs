//! Companion-agent personality channel.
//!
//! The tool/action Event facts in [`crate::agent_facts::agent_facts_from_messages`]
//! capture *what the agent did* — a tool call or a completed task. That is
//! enough for a task assistant, but a **companion AI** (e.g. an AI role-playing
//! 白流苏) needs to also remember *who the agent is*: the personality it reveals
//! through its own first-person speech across many turns.
//!
//! This module scans assistant messages for self-referential personality
//! phrases ("我是……的人", "我不要", "我心里……害怕") and turns each into a
//! typed fact (Identity / Emotion / Preference / Goal) attributed to the
//! **Agent** entity, tagged `attribution = "agent_personality"`. A negated
//! stance marker ("我不喜欢") stays a separate fact from its affirmative twin
//! ("我喜欢") so a like/dislike stance pair is never collapsed.
//!
//! Zero-pollution invariant: only assistant messages are scanned, so the
//! agent's persona never leaks into the User entity.

use crate::cognition::{Fact, FactType};
use crate::types::Message;

/// `attribution` value distinguishing persona facts from tool/action Event
/// facts in the agent channel.
pub const AGENT_PERSONALITY_ATTRIBUTION: &str = "agent_personality";

/// One self-referential personality signal a companion agent may express.
///
/// `negated` distinguishes stance-against statements ("我不肯低头") from
/// affirmative ones ("我喜欢安稳") — both are personality, but they must not
/// collide in the same preference bucket.
struct PersonalityMarker {
    marker: &'static str,
    fact_type: FactType,
    negated: bool,
}

/// Bilingual markers for agent first-person personality statements.
///
/// These mirror the user-path vocabulary in `conversation_compiler.rs` so the
/// agent channel speaks the same fact language, but are scoped to
/// **self-referential** phrases only (`我是`, `我不要`, `我心里`) — an agent
/// describing *itself* is identity/emotion/preference about the Agent entity,
/// never about the User (zero-pollution invariant).
const PERSONALITY_MARKERS: &[PersonalityMarker] = &[
    // Identity: who the agent is ("我是……的人", "我离过婚", "我是白流苏").
    PersonalityMarker {
        marker: "我是",
        fact_type: FactType::Identity,
        negated: false,
    },
    PersonalityMarker {
        marker: "我向来",
        fact_type: FactType::Identity,
        negated: false,
    },
    PersonalityMarker {
        marker: "我过了时",
        fact_type: FactType::Identity,
        negated: false,
    },
    // Emotion: what the agent feels ("我心里", "心里", "我害怕", "我难过").
    // `心里` alone is included because "心里也有害怕的时候" does not always
    // keep 我 immediately before 心里, yet still expresses the agent's feeling
    // (this channel scans assistant messages only).
    PersonalityMarker {
        marker: "我心里",
        fact_type: FactType::Emotion,
        negated: false,
    },
    PersonalityMarker {
        marker: "心里",
        fact_type: FactType::Emotion,
        negated: false,
    },
    PersonalityMarker {
        marker: "我害怕",
        fact_type: FactType::Emotion,
        negated: false,
    },
    PersonalityMarker {
        marker: "我难过",
        fact_type: FactType::Emotion,
        negated: false,
    },
    PersonalityMarker {
        marker: "我羡慕",
        fact_type: FactType::Emotion,
        negated: false,
    },
    PersonalityMarker {
        marker: "我恨",
        fact_type: FactType::Emotion,
        negated: false,
    },
    // Preference: what the agent likes/loathes ("我喜欢", "我不喜欢").
    PersonalityMarker {
        marker: "我喜欢",
        fact_type: FactType::Preference,
        negated: false,
    },
    PersonalityMarker {
        marker: "我偏爱",
        fact_type: FactType::Preference,
        negated: false,
    },
    PersonalityMarker {
        marker: "我爱",
        fact_type: FactType::Preference,
        negated: false,
    },
    PersonalityMarker {
        marker: "我不喜欢",
        fact_type: FactType::Preference,
        negated: true,
    },
    PersonalityMarker {
        marker: "我讨厌",
        fact_type: FactType::Preference,
        negated: true,
    },
    // Goal / stance: what the agent wants or refuses ("我要", "我不要", "我不肯").
    PersonalityMarker {
        marker: "我要",
        fact_type: FactType::Goal,
        negated: false,
    },
    PersonalityMarker {
        marker: "我不要",
        fact_type: FactType::Goal,
        negated: true,
    },
    PersonalityMarker {
        marker: "我不肯",
        fact_type: FactType::Goal,
        negated: true,
    },
    PersonalityMarker {
        marker: "我宁可",
        fact_type: FactType::Goal,
        negated: false,
    },
];

/// Extract the companion agent's **persona** from its natural speech.
///
/// Every fact is attributed to the **Agent** entity, tagged
/// `attribution = "agent_personality"`, and tagged `negated` when the marker
/// is a stance-against phrase, so consumers can keep a like/dislike stance
/// pair separate without losing the signal.
#[must_use]
pub fn agent_personality_facts_from_messages(
    messages: &[Message],
    agent_entity_id: i64,
    logical_time: i32,
) -> Vec<Fact> {
    let mut facts = Vec::new();
    for msg in messages {
        if !msg.is_assistant() {
            continue;
        }
        // Prefer the longest matching marker so the stance-against phrase wins:
        // "我不喜欢" (negated) must beat the plain "我喜欢" it contains, and
        // "我不要" must beat "我要". Shorter markers are only a fallback.
        let mut matched: Option<&PersonalityMarker> = None;
        for marker in PERSONALITY_MARKERS {
            if msg.content.contains(marker.marker)
                && matched.is_none_or(|m| marker.marker.len() > m.marker.len())
            {
                matched = Some(marker);
            }
        }
        let Some(marker) = matched else {
            continue;
        };
        facts.push(Fact {
            id: None,
            entity_id: agent_entity_id,
            fact_type: marker.fact_type,
            time: logical_time,
            payload: serde_json::json!({
                "attribution": AGENT_PERSONALITY_ATTRIBUTION,
                "content": msg.content,
                "negated": marker.negated,
            }),
            evidence_id: None,
            created_at: i64::from(logical_time),
            ..Fact::default()
        });
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_facts::agent_facts_from_messages;

    /// Objective: Verify a companion agent's first-person personality speech
    /// produces a typed fact attributed to the Agent entity with the
    /// `agent_personality` marker.
    /// Invariants: "我是离过婚的人" → one Identity fact; entity_id is the agent;
    /// attribution is "agent_personality"; not negated.
    #[test]
    fn agent_personality_extracts_identity_statement() {
        let messages = vec![Message::new(
            "assistant",
            "我是白流苏，离过婚，爱过，也输过。可我还活着。",
        )];
        let facts = agent_personality_facts_from_messages(&messages, 77, 2_026_073_000);
        assert_eq!(
            facts.len(),
            1,
            "one first-person identity phrase → one fact"
        );
        let fact = &facts[0];
        assert_eq!(fact.entity_id, 77, "persona fact is about the Agent entity");
        assert_eq!(
            fact.fact_type,
            FactType::Identity,
            "identity phrase → Identity"
        );
        assert_eq!(
            fact.payload["attribution"], "agent_personality",
            "persona facts carry the agent_personality marker"
        );
        assert_eq!(
            fact.payload["negated"], false,
            "an affirmative identity statement is not negated"
        );
    }

    /// Objective: Verify the longest-marker rule — a stance-against phrase
    /// ("我不喜欢") wins over the plain marker ("我喜欢") it contains.
    /// Invariants: one Preference fact; negated == true; payload content is the
    /// full sentence.
    #[test]
    fn agent_personality_negated_preference_beats_plain() {
        let messages = vec![Message::new("assistant", "我不喜欢虚伪的应酬，太累了。")];
        let facts = agent_personality_facts_from_messages(&messages, 3, 1);
        assert_eq!(facts.len(), 1, "the stance-against phrase is the signal");
        assert_eq!(facts[0].fact_type, FactType::Preference);
        assert_eq!(
            facts[0].payload["negated"], true,
            "negated marker must be preserved so like/dislike stay separate"
        );
        assert!(
            facts[0].payload["content"]
                .as_str()
                .unwrap_or("")
                .contains("我不喜欢"),
            "full sentence is retained as evidence"
        );
    }

    /// Objective: Verify emotion and stance markers map to the right types and
    /// that user messages never feed the agent persona channel.
    /// Invariants: "我心里……害怕" → Emotion; "我不要你为我改什么" → Goal
    /// (negated); a user message alone yields no persona facts.
    #[test]
    fn agent_personality_maps_emotion_and_stance_and_ignores_users() {
        let messages = vec![
            Message::new("user", "我心里很害怕"),
            Message::new("assistant", "我一个人走了这些年，心里也有害怕的时候。"),
            Message::new("assistant", "我不要你为我改什么。"),
        ];
        let facts = agent_personality_facts_from_messages(&messages, 9, 1);

        let emotions: Vec<&Fact> = facts
            .iter()
            .filter(|f| f.fact_type == FactType::Emotion)
            .collect();
        assert_eq!(emotions.len(), 1, "one emotion-bearing agent line");
        assert_eq!(emotions[0].payload["negated"], false);

        let goals: Vec<&Fact> = facts
            .iter()
            .filter(|f| f.fact_type == FactType::Goal)
            .collect();
        assert_eq!(goals.len(), 1, "one stance/goal agent line");
        assert_eq!(
            goals[0].payload["negated"], true,
            "我不要 is a stance-against goal"
        );
        assert!(
            facts.iter().all(|f| f.entity_id == 9),
            "all persona facts belong to the Agent entity, never the user"
        );
    }

    /// Objective: Verify a companion-style agent line that carries BOTH a
    /// completion marker and personality speech yields a tool/action Event AND
    /// a persona fact without interference.
    /// Invariants: agent_facts_from_messages gives the Event; the persona
    /// channel gives the Identity/Preference fact; both target the agent.
    #[test]
    fn personality_channel_coexists_with_action_events() {
        let messages = vec![Message::new(
            "assistant",
            "Done, 我已经处理好了。我是白流苏，我宁可一个人走夜路。",
        )];
        let action_facts = agent_facts_from_messages(&messages, 5, 1);
        assert_eq!(
            action_facts.len(),
            1,
            "completion marker still yields an Event fact"
        );
        let persona_facts = agent_personality_facts_from_messages(&messages, 5, 1);
        assert!(!persona_facts.is_empty(), "persona channel also fires");
        assert!(
            persona_facts.iter().all(|f| f.entity_id == 5),
            "persona facts stay on the Agent entity"
        );
    }
}
