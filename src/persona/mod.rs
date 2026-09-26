//! Persona signal extraction layer — the perception core for "persona
//! consistency".
//!
//! This module replaces the hard-coded keyword matching in
//! [`crate::agent_personality`] with a pluggable [`PersonaSignalExtractor`]
//! trait. Two concrete implementations are provided:
//!
//! - [`EmbeddingPersonaExtractor`] — the primary path. It embeds prototype
//!   sentences (loaded from a JSON config) and the speaker's utterance, then
//!   picks the best-matching `(fact_type, negated)` prototype via cosine
//!   similarity with an argmax threshold. No LLM is involved.
//! - [`KeywordPersonaExtractor`] — the offline fallback. It mirrors the
//!   existing `PERSONALITY_MARKERS` keyword logic so behaviour is consistent
//!   when the embedding backend is unavailable.
//!
//! Design invariants:
//! - **No LLM**: every decision is a pure vector / keyword computation.
//! - **Speaker attribution**: signals are attributed to the speaking entity,
//!   not always the agent. See [`SpeakerAttribution`].
//! - **mem0 v3 ADD-only accumulation**: the [`reconciler`] never deletes or
//!   overwrites a stored fact. It returns [`ReconcileDecision::Add`] for new
//!   or transitioning facts and [`ReconcileDecision::Noop`] only for exact
//!   semantic duplicates.

pub mod check;
pub mod embedding_extractor;
pub mod inject;
pub mod keyword_extractor;
pub mod prototype;
pub mod reconciler;
pub mod speaker_attribution;
pub mod timeline;

#[cfg(test)]
mod integration_tests;

pub use check::{PersonaCheckEngine, PersonaCheckResult, filter_persona_facts, is_persona_fact};
pub use embedding_extractor::EmbeddingPersonaExtractor;
pub use inject::{
    PersonaCard, build_persona_card_from_facts, load_persona_cards, lookup_persona_card,
    merge_persona_card_file,
};
pub use keyword_extractor::KeywordPersonaExtractor;
pub use prototype::{
    PersonaPrototypeConfig, PersonaPrototypeEntry, PersonaPrototypes, PersonaThresholds,
    PrototypeVectorCache, load_prototype_config,
};
pub use reconciler::{
    ReconcileDecision, ReconcileOutcome, Reconciler, ReconcilerConfig, TransitionMeta,
};
pub use speaker_attribution::{Speaker, SpeakerAttribution};
pub use timeline::{
    EvolutionTimeline, Milestone, MilestoneType, build_evolution_timeline,
    build_timeline_for_entity,
};

use crate::cognition::{Fact, FactType};
use crate::error::Result;

/// A single extracted persona signal, before attribution or storage.
///
/// `negated` distinguishes stance-against statements ("我不喜欢应酬") from
/// their affirmative twins ("我喜欢安稳"). Both are persona, but they must
/// not collide in the same preference bucket — the reconciler uses this flag
/// to detect stance-flip transitions.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonaSignal {
    /// The original utterance text the signal was extracted from.
    pub text: String,
    /// Which facet of persona this signal expresses.
    pub fact_type: FactType,
    /// Whether the signal is a stance-against (negated) statement.
    pub negated: bool,
    /// Cosine similarity to the winning prototype (semantic path only).
    /// The keyword path leaves this as `0.0` since it does not compute
    /// similarity.
    pub confidence: f32,
}

/// Attribution metadata for a persona signal: who said it, and which entity
/// the resulting fact should be attached to.
///
/// This is the "speaker → entity" mapping that makes the extractor work on
/// mixed corpora (novels with many characters, real companion dialogs with
/// distinct agent / user entities).
///
/// `SignalAttribution` is a re-export of [`SpeakerAttribution`] — the two
/// names refer to the same type, so callers can use whichever reads best at
/// the call site.
pub type SignalAttribution = SpeakerAttribution;

/// Extract persona signals from a single speaker's utterance.
///
/// Implementations must be deterministic and side-effect free. The trait is
/// async because the semantic path may issue remote embedding calls; the
/// keyword path is synchronous in practice and simply awaits a ready future.
///
/// # Returns
///
/// Zero or more [`PersonaSignal`]s. An empty vec means the utterance carried
/// no recognisable persona signal — this is a normal outcome, not an error.
#[async_trait::async_trait]
pub trait PersonaSignalExtractor: Send + Sync {
    /// Extract persona signals from `utterance`.
    ///
    /// `utterance` is the raw text of a single speaker's turn. The extractor
    /// scans it for self-referential personality statements and returns every
    /// distinct signal it can identify.
    async fn extract(&self, utterance: &str) -> Result<Vec<PersonaSignal>>;

    /// Whether this extractor is the semantic (embedding) path.
    ///
    /// Used by callers to log which path produced a signal. The keyword
    /// fallback returns `false`.
    fn is_semantic(&self) -> bool;
}

/// Build a [`Fact`] from a persona signal and attribution.
///
/// The fact payload carries the persona attribution marker, the original
/// utterance, the `negated` flag, and the extractor confidence. This keeps the
/// fact self-describing for downstream consumers (persona_check, timeline).
///
/// # Arguments
///
/// * `signal` - The extracted persona signal.
/// * `attribution` - Speaker attribution and target entity.
/// * `logical_time` - The logical turn time, used as `fact.time`.
///
/// # Returns
///
/// A [`Fact`] ready for insertion into the fact store.
#[must_use]
pub fn signal_to_fact(
    signal: &PersonaSignal,
    attribution: &SignalAttribution,
    logical_time: i64,
) -> Fact {
    Fact {
        id: None,
        entity_id: attribution.entity_id,
        fact_type: signal.fact_type,
        time: logical_time,
        // Column confidence carries the signal score so consumers reading
        // `fact.confidence` (provenance, ranking) see the real value —
        // payload-only left it at Fact::default 1.0.
        confidence: f64::from(signal.confidence).clamp(0.0, 1.0),
        payload: serde_json::json!({
            "attribution": crate::agent_personality::AGENT_PERSONALITY_ATTRIBUTION,
            "speaker": attribution.speaker.as_str(),
            "content": signal.text,
            "negated": signal.negated,
            "confidence": signal.confidence,
        }),
        evidence_id: None,
        created_at: logical_time,
        ..Fact::default()
    }
}
