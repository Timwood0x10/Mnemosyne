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
        let user_facts = crate::conversation_compiler::compile_user_facts(
            messages,
            user_entity_id,
            logical_time,
        );
        let agent_facts = agent_facts_from_messages(messages, agent_entity_id, logical_time);
        let derived_facts = derived_facts_from_messages(messages, user_entity_id, logical_time);
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
}
