//! Persona consistency guard — the "人设不崩" conflict/drift detector.
//!
//! This module powers the `persona_check` MCP tool. Given the agent's draft
//! reply and the accumulated persona facts for that agent, it decides whether
//! the reply:
//!
//! - **contradicts** an established persona fact ([`PersonaConflict`]) — same
//!   `fact_type`, opposite `negated`, high similarity; or
//! - **drifts** from the persona ([`PersonaDrift`]) by introducing a statement
//!   with no anchor in the stored persona.
//!
//! No LLM: the semantic path embeds the draft and each stored persona fact and
//! compares cosine similarity. When the embedding backend is unavailable the
//! keyword path falls back to negation + shared-bigram overlap so the guard
//! stays usable offline.
//!
//! Reconciliation policy: this is a **check-only** guard. It never writes,
//! overwrites, or deletes facts — v3 ADD-only accumulation is a separate
//! concern handled by [`crate::persona::reconciler`].

use std::sync::Arc;

use crate::agent_personality::AGENT_PERSONALITY_ATTRIBUTION;
use crate::cognition::{Fact, FactType};
use crate::embed::EmbeddingService;
use crate::error::Result;
use crate::persona::embedding_extractor::EmbeddingPersonaExtractor;
use crate::persona::keyword_extractor::KeywordPersonaExtractor;
use crate::persona::prototype::{PersonaThresholds, PrototypeVectorCache};
use crate::persona::{PersonaSignal, PersonaSignalExtractor};

/// A detected contradiction between the draft and an established persona fact.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonaConflict {
    /// The persona facet under test.
    pub fact_type: FactType,
    /// The persona signal extracted from the draft reply.
    pub draft_signal: PersonaSignal,
    /// The stored fact that is contradicted (its `id`).
    pub stored_fact_id: i64,
    /// The original text of the contradicted stored fact.
    pub stored_content: String,
    /// Whether the stored fact is a stance-against statement.
    pub stored_negated: bool,
    /// How similar the draft and the stored fact are (cosine for the semantic
    /// path, shared-bigram count for the keyword path).
    pub similarity: f32,
}

/// A persona statement in the draft with no anchor in the stored persona.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonaDrift {
    /// The persona facet under test.
    pub fact_type: FactType,
    /// The persona signal extracted from the draft reply.
    pub draft_signal: PersonaSignal,
    /// Why the statement was considered unanchored.
    pub reason: String,
}

/// The outcome of checking one draft reply against the stored persona.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PersonaCheckResult {
    /// Direct contradictions the host should revise before replying.
    pub conflicts: Vec<PersonaConflict>,
    /// Unanchored persona statements the host should treat cautiously.
    pub drift: Vec<PersonaDrift>,
    /// Signals that matched an established persona fact (no action needed).
    pub consistent_count: usize,
    /// Total persona signals extracted from the draft.
    pub total_signals: usize,
}

impl PersonaCheckResult {
    /// Whether the draft introduced neither conflicts nor drift.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty() && self.drift.is_empty()
    }
}

/// The persona-consistency guard engine.
///
/// `cache` is `Some` when the embedding backend is available (semantic path);
/// `None` forces the keyword fallback. Extraction and matching both use the
/// same backend so the two paths are consistent.
pub struct PersonaCheckEngine {
    cache: Option<PrototypeVectorCache>,
    embedder: Arc<dyn EmbeddingService>,
    thresholds: PersonaThresholds,
}

impl PersonaCheckEngine {
    /// Construct a new guard.
    ///
    /// Pass `cache = Some(...)` to enable the semantic path, or `None` to use
    /// the offline keyword fallback.
    #[must_use]
    pub fn new(
        cache: Option<PrototypeVectorCache>,
        embedder: Arc<dyn EmbeddingService>,
        thresholds: PersonaThresholds,
    ) -> Self {
        Self {
            cache,
            embedder,
            thresholds,
        }
    }

    /// Check `draft` (the agent's reply) against the stored persona facts.
    ///
    /// `stored` is the full fact list for the agent entity; only facts tagged
    /// `attribution == "agent_personality"` participate in the comparison.
    /// The function does not error when the draft carries no persona signal —
    /// it returns an empty result instead.
    pub async fn check(&self, draft: &str, stored: &[Fact]) -> Result<PersonaCheckResult> {
        let mut result = PersonaCheckResult::default();

        let signals = self.extract_signals(draft).await?;
        result.total_signals = signals.len();
        if signals.is_empty() {
            return Ok(result);
        }

        let persona: Vec<&Fact> = stored.iter().filter(|f| is_persona_fact(f)).collect();

        for signal in &signals {
            self.check_signal(signal, &persona, &mut result).await?;
        }
        Ok(result)
    }

    /// Extract persona signals from the draft, preferring the semantic path.
    async fn extract_signals(&self, draft: &str) -> Result<Vec<PersonaSignal>> {
        if let Some(cache) = &self.cache {
            let extractor =
                EmbeddingPersonaExtractor::new(cache, self.embedder.as_ref(), self.thresholds);
            extractor.extract(draft).await
        } else {
            let extractor = KeywordPersonaExtractor::new();
            extractor.extract(draft).await
        }
    }

    /// Check a single signal against the same-type persona facts.
    async fn check_signal(
        &self,
        signal: &PersonaSignal,
        persona: &[&Fact],
        result: &mut PersonaCheckResult,
    ) -> Result<()> {
        let same_type: Vec<&&Fact> = persona
            .iter()
            .filter(|f| f.fact_type == signal.fact_type)
            .collect();
        if same_type.is_empty() {
            result.drift.push(PersonaDrift {
                fact_type: signal.fact_type,
                draft_signal: signal.clone(),
                reason: "no stored persona fact of this type".into(),
            });
            return Ok(());
        }

        if self.cache.is_some() {
            self.check_semantic(signal, &same_type, result).await
        } else {
            self.check_keyword(signal, &same_type, result);
            Ok(())
        }
    }

    /// Semantic path: cosine similarity between the draft and each stored fact.
    async fn check_semantic(
        &self,
        signal: &PersonaSignal,
        same_type: &[&&Fact],
        result: &mut PersonaCheckResult,
    ) -> Result<()> {
        let draft_vec = self.embedder.embed(&signal.text).await?;
        if draft_vec.is_empty() {
            return Ok(());
        }

        let mut best_same_negated: Option<f32> = None;
        let mut best_conflict: Option<PersonaConflict> = None;

        for fact in same_type {
            let content = fact
                .payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if content.is_empty() {
                continue;
            }
            let stored_negated = fact
                .payload
                .get("negated")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let fvec = self.embedder.embed(content).await?;
            if fvec.is_empty() {
                continue;
            }
            let sim = cosine(&draft_vec, &fvec);

            if stored_negated != signal.negated && sim >= self.thresholds.conflict {
                let candidate = PersonaConflict {
                    fact_type: signal.fact_type,
                    draft_signal: signal.clone(),
                    stored_fact_id: fact.id.unwrap_or(-1),
                    stored_content: content.to_string(),
                    stored_negated,
                    similarity: sim,
                };
                if best_conflict.as_ref().is_none_or(|c| sim > c.similarity) {
                    best_conflict = Some(candidate);
                }
            } else if stored_negated == signal.negated {
                best_same_negated = Some(best_same_negated.map_or(sim, |b| b.max(sim)));
            }
        }

        if let Some(c) = best_conflict {
            result.conflicts.push(c);
        } else if best_same_negated.is_none_or(|b| b < self.thresholds.match_) {
            result.drift.push(PersonaDrift {
                fact_type: signal.fact_type,
                draft_signal: signal.clone(),
                reason: "no anchored persona fact (best similarity below match threshold)".into(),
            });
        } else {
            result.consistent_count += 1;
        }
        Ok(())
    }

    /// Offline keyword path: negation + shared-bigram overlap heuristic.
    fn check_keyword(
        &self,
        signal: &PersonaSignal,
        same_type: &[&&Fact],
        result: &mut PersonaCheckResult,
    ) {
        let mut best_same_negated: Option<usize> = None;
        let mut best_conflict: Option<PersonaConflict> = None;

        for fact in same_type {
            let content = fact
                .payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if content.is_empty() {
                continue;
            }
            let stored_negated = fact
                .payload
                .get("negated")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let overlap = shared_bigrams(&signal.text, content);

            // A single shared bigram is enough to confirm the two statements
            // touch the same topic: the marker verbs differ between a
            // stance-against signal and its stored affirmative twin (e.g.
            // "我讨厌" vs "我喜欢"), so the shared content is often just the
            // 2-char topic ("应酬"). Requiring two shared bigrams would miss
            // genuine contradictions on short topics.
            if stored_negated != signal.negated && overlap >= 1 {
                let candidate = PersonaConflict {
                    fact_type: signal.fact_type,
                    draft_signal: signal.clone(),
                    stored_fact_id: fact.id.unwrap_or(-1),
                    stored_content: content.to_string(),
                    stored_negated,
                    similarity: overlap as f32,
                };
                if best_conflict
                    .as_ref()
                    .is_none_or(|c| overlap > c.similarity as usize)
                {
                    best_conflict = Some(candidate);
                }
            } else if stored_negated == signal.negated {
                best_same_negated = Some(best_same_negated.map_or(overlap, |b| b.max(overlap)));
            }
        }

        if let Some(c) = best_conflict {
            result.conflicts.push(c);
        } else if best_same_negated.is_none_or(|b| b == 0) {
            result.drift.push(PersonaDrift {
                fact_type: signal.fact_type,
                draft_signal: signal.clone(),
                reason: "no anchored persona fact (zero shared bigrams)".into(),
            });
        } else {
            result.consistent_count += 1;
        }
    }
}

/// Whether a fact is a persona fact (tagged `attribution == "agent_personality"`).
#[must_use]
pub fn is_persona_fact(fact: &Fact) -> bool {
    fact.payload
        .get("attribution")
        .and_then(serde_json::Value::as_str)
        == Some(AGENT_PERSONALITY_ATTRIBUTION)
}

/// Filter `facts` down to persona facts only.
#[must_use]
pub fn filter_persona_facts(facts: &[Fact]) -> Vec<&Fact> {
    facts.iter().filter(|f| is_persona_fact(f)).collect()
}

/// Cosine similarity in `[0.0, 1.0]` for same-length nonzero vectors.
///
/// Returns `0.0` on dimension mismatch or zero-magnitude vectors so the caller
/// treats them as maximally dissimilar (no false conflicts).
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        return 0.0;
    }
    (dot / denom).clamp(0.0, 1.0)
}

/// Count shared character-bigrams between two strings (offline heuristic).
fn shared_bigrams(a: &str, b: &str) -> usize {
    if a.chars().count() < 2 || b.chars().count() < 2 {
        return 0;
    }
    let set: std::collections::HashSet<String> = char_bigrams(a).into_iter().collect();
    char_bigrams(b)
        .into_iter()
        .filter(|bg| set.contains(bg))
        .count()
}

/// Enumerate the character-bigrams of a string.
fn char_bigrams(s: &str) -> Vec<String> {
    s.chars()
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::persona::prototype::{
        PersonaPrototypeEntry, PersonaPrototypes, PersonaThresholds, PrototypeVectorCache,
    };

    /// A stub embedder returning deterministic vectors for testing.
    struct StubEmbedder;

    #[async_trait::async_trait]
    impl EmbeddingService for StubEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            let v = match text {
                "我喜欢安稳" => vec![0.0, 1.0, 0.0],
                "我讨厌应酬" => vec![0.9, 0.1, 0.0],
                "我喜欢应酬" => vec![0.9, 0.1, 0.0],
                "我是白流苏" => vec![1.0, 0.0, 0.0],
                // Full draft strings embed near their nearest prototype so the
                // semantic path (engine() uses a cache) extracts a signal.
                "我讨厌应酬，太累了。" => vec![0.9, 0.1, 0.0],
                "我喜欢安稳踏实。" => vec![0.0, 1.0, 0.0],
                "我是白流苏，离过婚。" => vec![1.0, 0.0, 0.0],
                _ => vec![0.5, 0.5, 0.5],
            };
            Ok(v)
        }
        async fn embed_with_prefix(&self, text: &str, _prefix: &str) -> Result<Vec<f32>> {
            self.embed(text).await
        }
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            let mut out = Vec::with_capacity(texts.len());
            for t in texts {
                out.push(self.embed(t).await?);
            }
            Ok(out)
        }
        async fn health_check(&self) -> Result<()> {
            Ok(())
        }
        fn model(&self) -> &str {
            "stub"
        }
        fn timeout(&self) -> std::time::Duration {
            std::time::Duration::from_secs(1)
        }
        fn enabled(&self) -> bool {
            true
        }
    }

    fn thresholds() -> PersonaThresholds {
        PersonaThresholds {
            match_: 0.75,
            dedup: 0.95,
            conflict: 0.75,
        }
    }

    async fn engine() -> PersonaCheckEngine {
        let embedder = Arc::new(StubEmbedder);
        let config = PersonaPrototypes {
            version: 1,
            thresholds: thresholds(),
            prototypes: vec![
                PersonaPrototypeEntry {
                    fact_type: FactType::Preference,
                    negated: false,
                    sentences: vec!["我喜欢安稳".to_string()],
                },
                PersonaPrototypeEntry {
                    fact_type: FactType::Preference,
                    negated: true,
                    sentences: vec!["我讨厌应酬".to_string()],
                },
                PersonaPrototypeEntry {
                    fact_type: FactType::Identity,
                    negated: false,
                    sentences: vec!["我是白流苏".to_string()],
                },
            ],
        };
        let cache = PrototypeVectorCache::build(&config, embedder.as_ref())
            .await
            .expect("semantic cache builds");
        PersonaCheckEngine::new(Some(cache), embedder, thresholds())
    }

    fn persona_fact(id: i64, fact_type: FactType, negated: bool, content: &str) -> Fact {
        Fact {
            id: Some(id),
            entity_id: 1,
            fact_type,
            time: 1,
            payload: serde_json::json!({
                "attribution": AGENT_PERSONALITY_ATTRIBUTION,
                "content": content,
                "negated": negated,
            }),
            evidence_id: None,
            created_at: 1,
        }
    }

    /// Objective: Verify a draft that contradicts a stored persona fact
    /// (opposite negated, high similarity) is reported as a conflict.
    /// Invariants: stored "我喜欢应酬" (negated=false) + draft "我讨厌应酬太累了"
    /// → one conflict; no drift.
    #[tokio::test]
    async fn detects_opposite_stance_conflict() {
        let engine = engine().await;
        let stored = vec![persona_fact(1, FactType::Preference, false, "我喜欢应酬")];
        let result = engine
            .check("我讨厌应酬，太累了。", &stored)
            .await
            .expect("check");
        assert_eq!(result.conflicts.len(), 1, "opposite stance → one conflict");
        let c = &result.conflicts[0];
        assert_eq!(c.stored_fact_id, 1, "conflict references the stored fact");
        assert!(
            !c.stored_negated,
            "stored fact kept its negation flag (affirmative)"
        );
        assert!(result.drift.is_empty(), "no drift alongside the conflict");
    }

    /// Objective: Verify a draft that repeats an established persona fact is
    /// consistent (no conflict, no drift).
    /// Invariants: stored "我喜欢安稳" + draft "我喜欢安稳踏实" → consistent.
    #[tokio::test]
    async fn consistent_repeat_is_not_flagged() {
        let engine = engine().await;
        let stored = vec![persona_fact(1, FactType::Preference, false, "我喜欢安稳")];
        let result = engine
            .check("我喜欢安稳踏实。", &stored)
            .await
            .expect("check");
        assert!(result.conflicts.is_empty(), "no conflict on repeat");
        assert!(result.drift.is_empty(), "no drift on repeat");
        assert_eq!(result.consistent_count, 1, "repeat counted as consistent");
    }

    /// Objective: Verify a persona statement with no anchor in the stored
    /// persona is reported as drift.
    /// Invariants: draft "我是白流苏" but stored has only a Preference fact →
    /// drift (no stored fact of this type).
    #[tokio::test]
    async fn unanchored_statement_is_drift() {
        let engine = engine().await;
        let stored = vec![persona_fact(1, FactType::Preference, false, "我喜欢安稳")];
        let result = engine
            .check("我是白流苏，离过婚。", &stored)
            .await
            .expect("check");
        assert!(result.conflicts.is_empty(), "no conflict for unanchored");
        assert_eq!(result.drift.len(), 1, "unanchored identity → drift");
    }

    /// Objective: Verify a draft with no persona signal yields an empty result.
    /// Invariants: "今天天气不错" → total_signals == 0, is_clean() == true.
    #[tokio::test]
    async fn no_signal_is_clean() {
        let engine = engine().await;
        let stored = vec![persona_fact(1, FactType::Preference, false, "我喜欢安稳")];
        let result = engine
            .check("今天天气不错。", &stored)
            .await
            .expect("check");
        assert_eq!(result.total_signals, 0, "no persona signal");
        assert!(result.is_clean(), "clean when no signal");
    }

    /// Objective: Verify the keyword fallback path detects an opposite-stance
    /// conflict through shared-bigram overlap.
    /// Invariants: cache=None; stored "我喜欢应酬" + draft "我讨厌应酬" → conflict.
    #[tokio::test]
    async fn keyword_fallback_detects_conflict() {
        let embedder = Arc::new(StubEmbedder);
        let engine = PersonaCheckEngine::new(None, embedder, thresholds());
        let stored = vec![persona_fact(1, FactType::Preference, false, "我喜欢应酬")];
        let result = engine
            .check("我讨厌应酬，太累了。", &stored)
            .await
            .expect("check");
        assert_eq!(result.conflicts.len(), 1, "keyword path flags the flip");
        assert_eq!(
            result.conflicts[0].stored_fact_id, 1,
            "references stored fact"
        );
    }

    /// Objective: Verify cosine similarity returns 0.0 on dimension mismatch.
    /// Invariants: cosine([1,2,3],[1,2]) → 0.0.
    #[test]
    fn cosine_dimension_mismatch_zero() {
        assert_eq!(cosine(&[1.0, 2.0, 3.0], &[1.0, 2.0]), 0.0);
    }

    /// Objective: Verify is_persona_fact only matches the agent_personality tag.
    /// Invariants: tagged fact → true; untagged fact → false.
    #[test]
    fn persona_fact_tagger() {
        let tagged = persona_fact(1, FactType::Identity, false, "我是白流苏");
        assert!(is_persona_fact(&tagged), "tagged fact is persona");
        let untagged = Fact {
            id: Some(1),
            entity_id: 1,
            fact_type: FactType::Identity,
            time: 1,
            payload: serde_json::json!({"content": "我是白流苏"}),
            evidence_id: None,
            created_at: 1,
        };
        assert!(!is_persona_fact(&untagged), "untagged fact is not persona");
    }

    /// Objective: Verify config has a prototype path constant for the guard.
    /// Invariants: the constant is non-empty.
    #[test]
    fn prototype_path_constant_defined() {
        assert!(!config::PERSONA_PROTOTYPES_PATH.is_empty());
    }
}
