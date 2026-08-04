//! Keyword-backed persona signal extractor — the offline fallback path.
//!
//! This extractor mirrors the legacy hard-coded keyword matching from
//! [`crate::agent_personality`] (the `PERSONALITY_MARKERS` table and its
//! longest-marker-wins rule) but adapts it to the new
//! [`PersonaSignalExtractor`] trait surface. It is used when the embedding
//! backend is unavailable ([`crate::embed::NullEmbedder`]) or fails at
//! runtime, so the system stays functional offline.
//!
//! Unlike the semantic path, the keyword path returns `confidence = 1.0`
//! for any matched marker — there is no graded similarity score.

use async_trait::async_trait;

use crate::cognition::FactType;
use crate::error::Result;
use crate::persona::{PersonaSignal, PersonaSignalExtractor};

/// One self-referential keyword marker for the offline fallback path.
///
/// `negated` distinguishes stance-against phrases ("我不喜欢") from their
/// affirmative twins ("我喜欢"). Both are persona, but they must not
/// collide in the same preference bucket.
#[derive(Debug, Clone, Copy, PartialEq)]
struct KeywordMarker {
    /// The literal substring to match (e.g. "我是", "我不喜欢").
    marker: &'static str,
    /// The persona facet this marker expresses.
    fact_type: FactType,
    /// Whether this marker is a stance-against (negated) phrase.
    negated: bool,
}

/// Bilingual keyword markers for first-person personality statements.
///
/// Order matters: longer markers are listed first so the
/// longest-marker-wins rule in [`extract`] naturally prefers the more
/// specific stance-against phrase over the shorter affirmative it contains
/// (e.g. "我不喜欢" must beat "我喜欢").
const KEYWORD_MARKERS: &[KeywordMarker] = &[
    // Preference (negated first so longest-wins picks it over 我喜欢)
    KeywordMarker {
        marker: "我不喜欢",
        fact_type: FactType::Preference,
        negated: true,
    },
    KeywordMarker {
        marker: "我讨厌",
        fact_type: FactType::Preference,
        negated: true,
    },
    KeywordMarker {
        marker: "我喜欢",
        fact_type: FactType::Preference,
        negated: false,
    },
    KeywordMarker {
        marker: "我偏爱",
        fact_type: FactType::Preference,
        negated: false,
    },
    KeywordMarker {
        marker: "我爱",
        fact_type: FactType::Preference,
        negated: false,
    },
    // Goal / stance (negated first)
    KeywordMarker {
        marker: "我不要",
        fact_type: FactType::Goal,
        negated: true,
    },
    KeywordMarker {
        marker: "我不肯",
        fact_type: FactType::Goal,
        negated: true,
    },
    KeywordMarker {
        marker: "我要",
        fact_type: FactType::Goal,
        negated: false,
    },
    KeywordMarker {
        marker: "我宁可",
        fact_type: FactType::Goal,
        negated: false,
    },
    // Identity
    KeywordMarker {
        marker: "我是",
        fact_type: FactType::Identity,
        negated: false,
    },
    KeywordMarker {
        marker: "我向来",
        fact_type: FactType::Identity,
        negated: false,
    },
    // Emotion
    KeywordMarker {
        marker: "我心里",
        fact_type: FactType::Emotion,
        negated: false,
    },
    KeywordMarker {
        marker: "心里",
        fact_type: FactType::Emotion,
        negated: false,
    },
    KeywordMarker {
        marker: "我害怕",
        fact_type: FactType::Emotion,
        negated: false,
    },
    KeywordMarker {
        marker: "我难过",
        fact_type: FactType::Emotion,
        negated: false,
    },
    KeywordMarker {
        marker: "我羡慕",
        fact_type: FactType::Emotion,
        negated: false,
    },
    KeywordMarker {
        marker: "我恨",
        fact_type: FactType::Emotion,
        negated: false,
    },
];

/// Keyword-based persona extractor — the offline fallback.
///
/// Construct with [`KeywordPersonaExtractor::new`]. The extractor holds no
/// state and is cheap to clone. It scans the utterance for the longest
/// matching keyword marker and emits a single [`PersonaSignal`] with
/// `confidence = 1.0`.
pub struct KeywordPersonaExtractor;

impl KeywordPersonaExtractor {
    /// Construct a new keyword extractor.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for KeywordPersonaExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PersonaSignalExtractor for KeywordPersonaExtractor {
    async fn extract(&self, utterance: &str) -> Result<Vec<PersonaSignal>> {
        let trimmed = utterance.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        // Longest-marker-wins: prefer the more specific stance-against phrase
        // over the shorter affirmative it contains (e.g. "我不喜欢" beats
        // "我喜欢"). This is a simple argmax by marker length.
        let mut matched: Option<&KeywordMarker> = None;
        for marker in KEYWORD_MARKERS {
            if trimmed.contains(marker.marker)
                && matched.is_none_or(|m| marker.marker.len() > m.marker.len())
            {
                matched = Some(marker);
            }
        }

        let Some(marker) = matched else {
            return Ok(Vec::new());
        };

        Ok(vec![PersonaSignal {
            text: trimmed.to_string(),
            fact_type: marker.fact_type,
            negated: marker.negated,
            confidence: 1.0,
        }])
    }

    fn is_semantic(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify the keyword extractor identifies an affirmative
    /// identity statement.
    /// Invariants: "我是白流苏" → one Identity signal, negated=false,
    /// confidence=1.0.
    #[tokio::test]
    async fn extracts_affirmative_identity() {
        let extractor = KeywordPersonaExtractor::new();
        let signals = extractor.extract("我是白流苏").await.expect("extract");
        assert_eq!(signals.len(), 1, "one signal for identity statement");
        let sig = &signals[0];
        assert_eq!(sig.fact_type, FactType::Identity, "classified as Identity");
        assert!(!sig.negated, "affirmative identity not negated");
        assert_eq!(sig.confidence, 1.0, "keyword match confidence is 1.0");
    }

    /// Objective: Verify the longest-marker-wins rule: "我不喜欢" (negated,
    /// 4 chars) must beat "我喜欢" (affirmative, 3 chars) even though the
    /// latter is a substring of the former.
    /// Invariants: "我不喜欢虚伪的应酬" → one Preference signal, negated=true.
    #[tokio::test]
    async fn negated_preference_beats_affirmative() {
        let extractor = KeywordPersonaExtractor::new();
        let signals = extractor
            .extract("我不喜欢虚伪的应酬，太累了。")
            .await
            .expect("extract");
        assert_eq!(signals.len(), 1, "one signal for negated preference");
        let sig = &signals[0];
        assert_eq!(
            sig.fact_type,
            FactType::Preference,
            "classified as Preference"
        );
        assert!(sig.negated, "stance-against preference marked negated");
    }

    /// Objective: Verify the longest-marker-wins rule for Goal: "我不要"
    /// (negated, 3 chars) must beat "我要" (affirmative, 2 chars).
    /// Invariants: "我不要你为我改什么" → one Goal signal, negated=true.
    #[tokio::test]
    async fn negated_goal_beats_affirmative() {
        let extractor = KeywordPersonaExtractor::new();
        let signals = extractor
            .extract("我不要你为我改什么。")
            .await
            .expect("extract");
        assert_eq!(signals.len(), 1, "one signal for negated goal");
        let sig = &signals[0];
        assert_eq!(sig.fact_type, FactType::Goal, "classified as Goal");
        assert!(sig.negated, "stance-against goal marked negated");
    }

    /// Objective: Verify emotion markers are recognized.
    /// Invariants: "我心里很害怕" → one Emotion signal, negated=false.
    #[tokio::test]
    async fn extracts_emotion() {
        let extractor = KeywordPersonaExtractor::new();
        let signals = extractor
            .extract("我心里很害怕，一个人走了这么多年。")
            .await
            .expect("extract");
        assert_eq!(signals.len(), 1, "one signal for emotion statement");
        let sig = &signals[0];
        assert_eq!(sig.fact_type, FactType::Emotion, "classified as Emotion");
        assert!(!sig.negated, "affirmative emotion not negated");
    }

    /// Objective: Verify that an utterance with no matching keyword markers
    /// produces no signal.
    /// Invariants: "今天天气不错" → 0 signals.
    #[tokio::test]
    async fn no_signal_for_unrelated_utterance() {
        let extractor = KeywordPersonaExtractor::new();
        let signals = extractor.extract("今天天气不错").await.expect("extract");
        assert!(signals.is_empty(), "unrelated utterance → no signal");
    }

    /// Objective: Verify that empty or whitespace-only utterances produce no
    /// signal.
    /// Invariants: "" and "   " → 0 signals.
    #[tokio::test]
    async fn empty_utterance_no_signal() {
        let extractor = KeywordPersonaExtractor::new();
        let signals = extractor.extract("").await.expect("extract");
        assert!(signals.is_empty(), "empty utterance → no signal");
        let signals = extractor.extract("   ").await.expect("extract");
        assert!(signals.is_empty(), "whitespace utterance → no signal");
    }

    /// Objective: Verify is_semantic() returns false for the keyword path.
    /// Invariants: is_semantic() == false.
    #[tokio::test]
    async fn is_semantic_flag() {
        let extractor = KeywordPersonaExtractor::new();
        assert!(
            !extractor.is_semantic(),
            "keyword extractor is not semantic"
        );
    }

    /// Objective: Verify the `心里` emotion marker works even without a
    /// preceding `我` (looser match for implicit emotion).
    /// Invariants: "心里也有害怕的时候" → one Emotion signal.
    #[tokio::test]
    async fn emotion_marker_without_prefix() {
        let extractor = KeywordPersonaExtractor::new();
        let signals = extractor
            .extract("心里也有害怕的时候")
            .await
            .expect("extract");
        assert_eq!(signals.len(), 1, "loose emotion marker matches");
        assert_eq!(
            signals[0].fact_type,
            FactType::Emotion,
            "classified as Emotion"
        );
    }
}
