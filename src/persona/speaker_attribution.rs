//! Speaker attribution — mapping each utterance to the entity that spoke it.
//!
//! The persona extractor must attribute signals to the **speaker's** entity,
//! not always the agent. This matters for two corpus types:
//!
//! - **Real companion dialogs**: `assistant` turns → Agent entity;
//!   `user` turns → User entity. The agent's self-description ("我是白流苏")
//!   belongs on the Agent; the user's self-description belongs on the User.
//! - **Novels with many characters**: each named character gets its own
//!   entity. A character's first-person speech ("我离过婚") is attributed to
//!   that character's entity.
//!
//! The [`SpeakerAttribution`] struct is the pre-resolved mapping from a
//! speaker role to a target entity id. Callers build it once per compilation
//! pass and feed it to [`crate::persona::signal_to_fact`].

use crate::types::Message;

/// The role of a speaker in a conversation.
///
/// This is a closed enum rather than a raw `&str` so the reconciler can
/// pattern-match exhaustively when deciding attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Speaker {
    /// The companion agent (assistant role in real dialogs).
    Agent,
    /// The human user (user role in real dialogs).
    User,
    /// A named character in a novel corpus.
    Character,
}

impl Speaker {
    /// Returns the string representation used in fact payloads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Speaker::Agent => "agent",
            Speaker::User => "user",
            Speaker::Character => "character",
        }
    }

    /// Resolve a [`Message`]'s role to a [`Speaker`].
    ///
    /// Assistant messages map to [`Speaker::Agent`], user messages to
    /// [`Speaker::User`]. Any other role (e.g. "system", "tool") returns
    /// `None` — those messages carry no first-person persona signal.
    #[must_use]
    pub fn from_message(msg: &Message) -> Option<Self> {
        if msg.is_assistant() {
            Some(Speaker::Agent)
        } else if msg.is_user() {
            Some(Speaker::User)
        } else {
            None
        }
    }
}

/// Pre-resolved mapping from a speaker to the entity id that should receive
/// the resulting persona facts.
///
/// Built once per compilation pass. The struct is intentionally simple
/// (two fields) so it can be passed by reference to
/// [`crate::persona::signal_to_fact`] without allocation.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerAttribution {
    /// The speaker role.
    pub speaker: Speaker,
    /// The entity id facts for this speaker should target.
    pub entity_id: i64,
}

impl SpeakerAttribution {
    /// Construct a new attribution.
    #[must_use]
    pub const fn new(speaker: Speaker, entity_id: i64) -> Self {
        Self { speaker, entity_id }
    }

    /// Build the attribution for an assistant message targeting the agent
    /// entity.
    #[must_use]
    pub const fn for_agent(agent_entity_id: i64) -> Self {
        Self::new(Speaker::Agent, agent_entity_id)
    }

    /// Build the attribution for a user message targeting the user entity.
    #[must_use]
    pub const fn for_user(user_entity_id: i64) -> Self {
        Self::new(Speaker::User, user_entity_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify Speaker::from_message correctly identifies agent
    /// messages.
    /// Invariants: assistant message → Some(Speaker::Agent).
    #[test]
    fn from_message_identifies_agent() {
        let msg = Message::new("assistant", "我是白流苏");
        assert_eq!(
            Speaker::from_message(&msg),
            Some(Speaker::Agent),
            "assistant message → Agent speaker"
        );
    }

    /// Objective: Verify Speaker::from_message correctly identifies user
    /// messages.
    /// Invariants: user message → Some(Speaker::User).
    #[test]
    fn from_message_identifies_user() {
        let msg = Message::new("user", "我喜欢这个建议");
        assert_eq!(
            Speaker::from_message(&msg),
            Some(Speaker::User),
            "user message → User speaker"
        );
    }

    /// Objective: Verify Speaker::from_message returns None for roles that
    /// carry no persona signal (system, tool, unknown).
    /// Invariants: "system", "tool", "narrator" → None.
    #[test]
    fn from_message_rejects_non_persona_roles() {
        let system_msg = Message::new("system", "You are a helpful assistant.");
        let tool_msg = Message::new("tool", "result: 42");
        let unknown_msg = Message::new("narrator", "她转过身去。");
        assert!(
            Speaker::from_message(&system_msg).is_none(),
            "system role → None"
        );
        assert!(
            Speaker::from_message(&tool_msg).is_none(),
            "tool role → None"
        );
        assert!(
            Speaker::from_message(&unknown_msg).is_none(),
            "unknown role → None"
        );
    }

    /// Objective: Verify Speaker::as_str returns the correct string for each
    /// variant.
    /// Invariants: Agent→"agent", User→"user", Character→"character".
    #[test]
    fn speaker_as_str() {
        assert_eq!(Speaker::Agent.as_str(), "agent");
        assert_eq!(Speaker::User.as_str(), "user");
        assert_eq!(Speaker::Character.as_str(), "character");
    }

    /// Objective: Verify SpeakerAttribution::for_agent builds the correct
    /// mapping.
    /// Invariants: speaker == Agent, entity_id preserved.
    #[test]
    fn for_agent_attribution() {
        let attr = SpeakerAttribution::for_agent(42);
        assert_eq!(attr.speaker, Speaker::Agent, "speaker is Agent");
        assert_eq!(attr.entity_id, 42, "entity_id preserved");
    }

    /// Objective: Verify SpeakerAttribution::for_user builds the correct
    /// mapping.
    /// Invariants: speaker == User, entity_id preserved.
    #[test]
    fn for_user_attribution() {
        let attr = SpeakerAttribution::for_user(99);
        assert_eq!(attr.speaker, Speaker::User, "speaker is User");
        assert_eq!(attr.entity_id, 99, "entity_id preserved");
    }

    /// Objective: Verify Speaker enum equality works correctly for use in
    /// HashMap keys.
    /// Invariants: Speaker::Agent == Speaker::Agent; Agent != User.
    #[test]
    fn speaker_equality() {
        assert_eq!(Speaker::Agent, Speaker::Agent, "Agent equals itself");
        assert_ne!(Speaker::Agent, Speaker::User, "Agent != User");
        assert_ne!(Speaker::User, Speaker::Character, "User != Character");
    }
}
