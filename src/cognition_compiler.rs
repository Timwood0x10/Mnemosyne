//! Unified cognition compiler for every production input source.
//!
//! The compiler emits the universal `Observation` IR and immutable `Fact`
//! records. `CompiledConversation` is retained only as a compatibility
//! projection for existing MCP response fields and prompt reconstruction.
//!
//! ## Three-state conversation facts (external-knowledge-plan §C)
//!
//! [`CognitionCompiler::compile_conversation_facts`] produces a
//! [`ConversationFacts`] container with three disjoint channels:
//!
//! - `user_facts` — first-hand user cognition (existing user-message path,
//!   zero agent pollution).
//! - `agent_facts` — `Event` facts for what the agent did, attributed to the
//!   **Agent** entity.
//! - `derived_facts` — agent restatements of user cognition, attributed to the
//!   User entity but marked `agent_derived` with discounted confidence.

use crate::agent_facts::{
    ConversationFacts, agent_facts_from_messages, derived_facts_from_messages,
};
use crate::agent_personality::agent_personality_facts_from_messages;
use crate::cognition::{Fact, Observation};
use crate::conversation_compiler::{
    ConversationCompiler, compile_user_observations, user_facts_from_observations,
};
use crate::types::{CompiledConversation, Message};

/// Complete output of one cognition compilation pass.
#[derive(Debug)]
pub struct CognitionCompileResult {
    /// Universal language-independent intermediate representation.
    pub observations: Vec<Observation>,
    /// Immutable event-sourced records derived from the observations.
    pub facts: Vec<Fact>,
    /// Compatibility projection for legacy knowledge, decisions, and session state.
    pub compatibility: CompiledConversation,
}

/// The single production compiler facade.
#[derive(Default)]
pub struct CognitionCompiler {
    compatibility: ConversationCompiler,
}

impl CognitionCompiler {
    /// Create a compiler with the compatibility projection enabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Compile conversation input through `Observation -> Fact` once.
    ///
    /// The compatibility projection remains read-only output. Persistence must
    /// use `facts`, preventing the legacy model from becoming a second source
    /// of truth.
    pub fn compile_conversation(
        &self,
        tenant_id: &str,
        messages: &[Message],
        user_entity_id: i64,
        logical_time: i32,
    ) -> CognitionCompileResult {
        let observations = compile_user_observations(messages, user_entity_id);
        let facts = user_facts_from_observations(&observations, logical_time);
        let compatibility = self.compatibility.compile(tenant_id, messages);

        CognitionCompileResult {
            observations,
            facts,
            compatibility,
        }
    }

    /// Compile a conversation into the three-state [`ConversationFacts`]
    /// container (external-knowledge-plan §C).
    ///
    /// - `user_facts`: first-hand user cognition, produced by the existing
    ///   user-message path (`compile_user_facts`). This channel is NEVER
    ///   polluted by agent or derived content (zero-pollution invariant).
    /// - `agent_facts`: `Event` facts for agent tool calls and completed
    ///   actions, attributed to `agent_entity_id` (the Agent entity, resolved
    ///   via [`crate::fact_store::SqliteFactStore::resolve_agent`]).
    /// - `derived_facts`: agent restatements of user cognition, attributed to
    ///   `user_entity_id` but marked `agent_derived` with discounted confidence
    ///   and double evidence.
    ///
    /// Callers persist each channel with the appropriate entity id and
    /// weighting; the `agent_fact_compile` MCP tool drives this entry point
    /// with `include_agent_facts = true`.
    #[must_use]
    pub fn compile_conversation_facts(
        &self,
        messages: &[Message],
        user_entity_id: i64,
        agent_entity_id: i64,
        logical_time: i32,
    ) -> ConversationFacts {
        let mut user_facts = crate::conversation_compiler::compile_user_facts(
            messages,
            user_entity_id,
            logical_time,
        );
        // Agent channel = what the agent did (tool/action Events) PLUS the
        // companion persona it reveals through first-person speech, so a
        // companion agent's personality accumulates turn after turn.
        let mut agent_facts = agent_facts_from_messages(messages, agent_entity_id, logical_time);
        agent_facts.extend(agent_personality_facts_from_messages(
            messages,
            agent_entity_id,
            logical_time,
        ));
        let derived_facts = derived_facts_from_messages(messages, user_entity_id, logical_time);

        // Companion signals (C1): wire the deterministic companion extractor
        // into the compile chain now that it has graduated from grayscale.
        // User-side signals merge into the user channel (no attribution marker,
        // preserving the zero-pollution invariant); assistant-side signals merge
        // into the agent channel tagged `agent_personality`.
        if !crate::knowledge::companion_extract::COMPANION_EXTRACT_GRAYSCALE {
            let extract = crate::knowledge::companion_extract::extract_companion_signals(messages);
            let (companion_user_facts, companion_agent_facts) =
                crate::knowledge::companion_extract::companion_facts_from_extract(
                    &extract,
                    messages,
                    user_entity_id,
                    agent_entity_id,
                    logical_time,
                );
            user_facts.extend(companion_user_facts);
            agent_facts.extend(companion_agent_facts);
        }

        ConversationFacts::from_channels(user_facts, agent_facts, derived_facts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::FactType;

    /// Objective: Verify the facade produces the unified IR and legacy projection together.
    /// Invariants: Facts target the requested user while compatibility fields remain populated.
    #[test]
    fn conversation_compiles_once_into_cognition_and_compatibility() {
        let messages = vec![
            Message::new("user", "I want to replace the parser module."),
            Message::new("assistant", "Implemented the parser replacement."),
        ];

        let result =
            CognitionCompiler::new().compile_conversation("tenant-a", &messages, 42, 2_026_073_000);

        assert!(
            !result.observations.is_empty(),
            "A user goal marker should cross the universal Observation IR"
        );
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.entity_id == 42 && fact.fact_type == FactType::Goal),
            "The immutable fact output should target the tenant-scoped user entity"
        );
        assert!(
            !result.compatibility.session.current_goal.is_empty(),
            "The legacy session projection must remain available without becoming a write path"
        );
    }

    /// Objective: Verify assistant-authored preferences never become user facts.
    /// Invariants: Only user messages contribute observations and persisted fact candidates.
    #[test]
    fn assistant_content_is_excluded_from_cognition_facts() {
        let messages = vec![Message::new(
            "assistant",
            "I like Rust and plan to rewrite the service.",
        )];

        let result =
            CognitionCompiler::new().compile_conversation("tenant-a", &messages, 42, 2_026_073_000);

        assert!(
            result.observations.is_empty(),
            "Assistant-authored content must not be attributed to the user Observation stream"
        );
        assert!(
            result.facts.is_empty(),
            "Assistant-authored content must not produce persisted user facts"
        );
    }

    /// Objective: Verify compile_conversation_facts populates all three channels
    /// with the correct entity attribution, and that the user channel stays
    /// unpolluted by agent/derived content (the zero-pollution invariant).
    /// Invariants: user_facts target the User entity and carry NO attribution
    /// marker; agent_facts target the Agent entity as Events; derived_facts
    /// target the User entity but are marked agent_derived.
    #[test]
    fn compile_conversation_facts_separates_three_channels() {
        let messages = vec![
            Message::new("user", "I want to learn Rust"),
            Message::new("assistant", "you want to learn Rust, great!"),
            Message::new("assistant", "Done, I created a Rust learning plan."),
        ];
        let result = CognitionCompiler::new().compile_conversation_facts(&messages, 42, 99, 100);

        // User channel: first-hand Goal fact, no attribution marker.
        assert!(
            result.user_facts.iter().all(|f| f.entity_id == 42),
            "user_facts target the User entity"
        );
        assert!(
            result
                .user_facts
                .iter()
                .all(|f| f.payload.get("attribution").is_none()),
            "user_facts carry no attribution marker (zero-pollution)"
        );
        assert!(
            result
                .user_facts
                .iter()
                .any(|f| f.fact_type == FactType::Goal),
            "user goal is captured first-hand"
        );

        // Agent channel: Event facts, Agent entity, attribution "agent".
        assert!(
            result
                .agent_facts
                .iter()
                .all(|f| f.entity_id == 99 && f.fact_type == FactType::Event),
            "agent_facts target the Agent entity as Events"
        );
        assert!(
            !result.agent_facts.is_empty(),
            "agent completion language captured"
        );

        // Derived channel: User entity, attribution "agent_derived".
        assert!(
            result.derived_facts.iter().all(|f| f.entity_id == 42
                && f.payload.get("attribution")
                    == Some(&serde_json::Value::String("agent_derived".into()))),
            "derived_facts target the User entity but are marked agent_derived"
        );
        assert!(
            !result.derived_facts.is_empty(),
            "agent restatement captured as a derived fact"
        );
    }

    /// Objective: Verify compile_conversation_facts with NO agent activity
    /// yields empty agent/derived channels but still populates user_facts —
    /// the agent channel is opt-in and never injects content on its own.
    /// Invariants: agent_facts and derived_facts empty; user_facts populated.
    #[test]
    fn compile_conversation_facts_without_agent_activity() {
        let messages = vec![Message::new("user", "I like Rust and plan to ship it.")];
        let result = CognitionCompiler::new().compile_conversation_facts(&messages, 7, 8, 1);
        assert!(
            !result.user_facts.is_empty(),
            "user facts still compiled from user messages"
        );
        assert!(
            result.agent_facts.is_empty(),
            "no assistant messages → no agent facts"
        );
        assert!(
            result.derived_facts.is_empty(),
            "no assistant messages → no derived facts"
        );
    }

    /// Objective: Verify a user-emitted companion emotion lands in the user
    /// channel and carries NO attribution marker (zero-pollution invariant).
    /// Invariants: the user Emotion fact targets user_entity_id; every
    /// user_facts fact has no attribution key.
    #[test]
    fn companion_user_emotion_goes_to_user_facts_without_attribution() {
        let messages = vec![Message::new("user", "今天被老板骂了，烦死了。")];
        let result = CognitionCompiler::new().compile_conversation_facts(&messages, 42, 99, 100);
        let companion_emotion: Vec<&Fact> = result
            .user_facts
            .iter()
            .filter(|f| f.fact_type == FactType::Emotion)
            .collect();
        assert!(
            !companion_emotion.is_empty(),
            "user emotion companion fact must be produced"
        );
        assert!(
            companion_emotion.iter().all(|f| f.entity_id == 42),
            "user-side companion emotion targets the User entity"
        );
        assert!(
            result
                .user_facts
                .iter()
                .all(|f| f.payload.get("attribution").is_none()),
            "user_facts carry no attribution marker (zero-pollution)"
        );
    }

    /// Objective: Verify an assistant-emitted self-cognition lands in the agent
    /// channel and is tagged `agent_personality` for the persona layer.
    /// Invariants: the agent Identity fact targets agent_entity_id and carries
    /// the agent_personality attribution marker.
    #[test]
    fn companion_assistant_self_cognition_goes_to_agent_facts_with_personality() {
        let messages = vec![Message::new(
            "assistant",
            "我是白流苏，我这个人学不会低头。",
        )];
        let result = CognitionCompiler::new().compile_conversation_facts(&messages, 42, 99, 100);
        let identity_facts: Vec<&Fact> = result
            .agent_facts
            .iter()
            .filter(|f| f.fact_type == FactType::Identity)
            .collect();
        assert!(
            !identity_facts.is_empty(),
            "assistant self-cognition companion fact must be produced"
        );
        assert!(
            identity_facts.iter().all(|f| f.payload.get("attribution")
                == Some(&serde_json::Value::String("agent_personality".into()))),
            "agent companion facts are tagged agent_personality"
        );
    }

    /// Objective: Verify wiring the companion extractor into the compile chain
    /// raises the total fact count above the non-companion baseline.
    /// Invariants: total() after compile exceeds the sum of the three
    /// non-companion channels.
    #[test]
    fn companion_compilation_increases_total_facts() {
        let messages = vec![
            Message::new("user", "今天被老板骂了，烦死了。你说我是不是太软弱。"),
            Message::new("assistant", "我夜里睡不着，心里发慌。我是白流苏。"),
        ];
        let baseline = agent_facts_from_messages(&messages, 99, 100).len()
            + agent_personality_facts_from_messages(&messages, 99, 100).len()
            + derived_facts_from_messages(&messages, 42, 100).len()
            + crate::conversation_compiler::compile_user_facts(&messages, 42, 100).len();
        let result = CognitionCompiler::new().compile_conversation_facts(&messages, 42, 99, 100);
        assert!(
            result.total() > baseline,
            "companion channels must raise the total above the baseline ({baseline})"
        );
    }
}
