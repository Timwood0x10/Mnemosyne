//! Unified cognition compiler for every production input source.
//!
//! The compiler emits the universal `Observation` IR and immutable `Fact`
//! records. `CompiledConversation` is retained only as a compatibility
//! projection for existing MCP response fields and prompt reconstruction.

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
}
