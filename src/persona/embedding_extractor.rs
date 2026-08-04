//! Embedding-backed persona signal extractor — the primary (semantic) path.
//!
//! Classification is a pure vector computation: embed the utterance, compute
//! cosine similarity against every cached prototype vector, and take the
//! argmax. If the winning similarity is at least [`PersonaThresholds::match_`]
//! the utterance is classified as `(fact_type, negated)`; otherwise it yields
//! no signal. No LLM is involved at any stage.
//!
//! The extractor borrows the [`PrototypeVectorCache`] and
//! [`EmbeddingService`] for its entire lifetime. The cache is built once at
//! startup; the embedder is called once per [`extract`] invocation.

use async_trait::async_trait;

use crate::embed::EmbeddingService;
use crate::error::Result;
use crate::persona::prototype::{PersonaThresholds, PrototypeVectorCache};
use crate::persona::{PersonaSignal, PersonaSignalExtractor};

/// Semantic persona extractor backed by an embedding service.
///
/// Construct with [`EmbeddingPersonaExtractor::new`] after building a
/// [`PrototypeVectorCache`]. The extractor is cheap to clone if the inner
/// references are shared.
pub struct EmbeddingPersonaExtractor<'a> {
    cache: &'a PrototypeVectorCache,
    embedder: &'a dyn EmbeddingService,
    thresholds: PersonaThresholds,
}

impl<'a> EmbeddingPersonaExtractor<'a> {
    /// Construct a new semantic extractor.
    ///
    /// # Arguments
    ///
    /// * `cache` - Pre-built prototype vector cache (borrowed for lifetime).
    /// * `embedder` - Embedding service used to embed utterances.
    /// * `thresholds` - Tunable cosine-similarity thresholds.
    #[must_use]
    pub fn new(
        cache: &'a PrototypeVectorCache,
        embedder: &'a dyn EmbeddingService,
        thresholds: PersonaThresholds,
    ) -> Self {
        Self {
            cache,
            embedder,
            thresholds,
        }
    }

    /// Cosine similarity in `[0.0, 1.0]` for same-length nonzero vectors.
    ///
    /// Returns `None` on dimension mismatch or zero-magnitude vectors so the
    /// caller can skip the prototype rather than treat it as maximally
    /// dissimilar.
    fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
        if a.len() != b.len() || a.is_empty() {
            return None;
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
            return None;
        }
        // Cosine can be slightly > 1 due to float rounding; clamp.
        Some((dot / denom).clamp(0.0, 1.0))
    }
}

#[async_trait]
impl<'a> PersonaSignalExtractor for EmbeddingPersonaExtractor<'a> {
    async fn extract(&self, utterance: &str) -> Result<Vec<PersonaSignal>> {
        let trimmed = utterance.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let utt_vec = self.embedder.embed(trimmed).await?;
        // Empty embedding (NullEmbedder or backend glitch) => no signal.
        if utt_vec.is_empty() {
            return Ok(Vec::new());
        }

        let prototypes = self.cache.vectors();
        let mut best_sim: f32 = -1.0;
        let mut best_idx: Option<usize> = None;
        for (i, proto) in prototypes.iter().enumerate() {
            let sim = match Self::cosine(&utt_vec, &proto.vector) {
                Some(s) => s,
                None => {
                    // Dimension mismatch between utterance embedding and this
                    // prototype's embedding. Skip rather than penalise.
                    continue;
                }
            };
            if sim > best_sim {
                best_sim = sim;
                best_idx = Some(i);
            }
        }

        let Some(idx) = best_idx else {
            return Ok(Vec::new());
        };
        // best_sim starts at -1.0; if every prototype mismatched dimensions,
        // best_sim could still be -1.0 and we'd incorrectly accept. Guard.
        if best_sim < 0.0 {
            return Ok(Vec::new());
        }

        let proto = &prototypes[idx];
        if best_sim < self.thresholds.match_ {
            // Below match threshold → no persona signal detected.
            return Ok(Vec::new());
        }

        Ok(vec![PersonaSignal {
            text: trimmed.to_string(),
            fact_type: proto.fact_type,
            negated: proto.negated,
            confidence: best_sim,
        }])
    }

    fn is_semantic(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::FactType;
    use crate::embed::EmbeddingService;
    use crate::error::Result;
    use crate::persona::prototype::{
        PersonaPrototypeEntry, PersonaPrototypes, PersonaThresholds, PrototypeVectorCache,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A stub embedder that returns deterministic vectors for testing.
    ///
    /// Each unique text is mapped to a fixed 3-dim vector. This lets us
    /// control cosine similarities precisely in tests.
    struct StubEmbedder {
        call_count: Arc<AtomicUsize>,
    }

    impl StubEmbedder {
        fn new() -> Self {
            Self {
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl EmbeddingService for StubEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            // Map known test sentences to specific vectors.
            let vec = match text {
                "我是白流苏" => vec![1.0, 0.0, 0.0],
                "我喜欢安稳" => vec![0.0, 1.0, 0.0],
                "我不喜欢虚伪的应酬" => vec![0.0, -1.0, 0.0],
                // Utterance under test - close to "我是白流苏"
                "我是白流苏，离过婚，爱过，也输过" => vec![0.95, 0.05, 0.0],
                // Unrelated utterance
                "今天天气不错" => vec![0.0, 0.0, 1.0],
                _ => vec![0.5, 0.5, 0.5],
            };
            Ok(vec)
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

    async fn build_test_cache(
        embedder: &dyn EmbeddingService,
    ) -> (PersonaPrototypes, PrototypeVectorCache) {
        let config = PersonaPrototypes {
            version: 1,
            thresholds: PersonaThresholds::default(),
            prototypes: vec![
                PersonaPrototypeEntry {
                    fact_type: FactType::Identity,
                    negated: false,
                    sentences: vec!["我是白流苏".to_string()],
                },
                PersonaPrototypeEntry {
                    fact_type: FactType::Preference,
                    negated: false,
                    sentences: vec!["我喜欢安稳".to_string()],
                },
                PersonaPrototypeEntry {
                    fact_type: FactType::Preference,
                    negated: true,
                    sentences: vec!["我不喜欢虚伪的应酬".to_string()],
                },
            ],
        };
        let cache = PrototypeVectorCache::build(&config, embedder)
            .await
            .expect("build cache");
        (config, cache)
    }

    /// Objective: Verify a high-similarity utterance is classified as the
    /// matching prototype's fact_type and negated flag.
    /// Invariants: "我是白流苏，离过婚..." (cosine≈0.997 with prototype) yields
    /// one Identity signal, negated=false, confidence ≥ match threshold.
    #[tokio::test]
    async fn extracts_matching_signal() {
        let embedder = StubEmbedder::new();
        let (config, cache) = build_test_cache(&embedder).await;
        let extractor = EmbeddingPersonaExtractor::new(&cache, &embedder, config.thresholds);

        let signals = extractor
            .extract("我是白流苏，离过婚，爱过，也输过")
            .await
            .expect("extract");
        assert_eq!(
            signals.len(),
            1,
            "one persona signal extracted from close-match utterance"
        );
        let sig = &signals[0];
        assert_eq!(sig.fact_type, FactType::Identity, "classified as Identity");
        assert!(!sig.negated, "affirmative identity statement");
        assert!(
            sig.confidence >= config.thresholds.match_,
            "confidence {} meets match threshold {}",
            sig.confidence,
            config.thresholds.match_
        );
    }

    /// Objective: Verify that an utterance with low similarity to all
    /// prototypes produces no signal (empty vec, not an error).
    /// Invariants: "今天天气不错" (orthogonal to all prototypes) → 0 signals.
    #[tokio::test]
    async fn no_signal_for_unrelated_utterance() {
        let embedder = StubEmbedder::new();
        let (config, cache) = build_test_cache(&embedder).await;
        let extractor = EmbeddingPersonaExtractor::new(&cache, &embedder, config.thresholds);

        let signals = extractor.extract("今天天气不错").await.expect("extract");
        assert!(
            signals.is_empty(),
            "unrelated utterance yields no persona signal"
        );
    }

    /// Objective: Verify that a negated prototype sentence correctly produces
    /// a signal with negated=true.
    /// Invariants: "我不喜欢虚伪的应酬" → Preference signal, negated=true.
    #[tokio::test]
    async fn extracts_negated_signal() {
        let embedder = StubEmbedder::new();
        let (config, cache) = build_test_cache(&embedder).await;
        let extractor = EmbeddingPersonaExtractor::new(&cache, &embedder, config.thresholds);

        let signals = extractor
            .extract("我不喜欢虚伪的应酬")
            .await
            .expect("extract");
        assert_eq!(
            signals.len(),
            1,
            "negated prototype sentence matches itself"
        );
        let sig = &signals[0];
        assert_eq!(
            sig.fact_type,
            FactType::Preference,
            "classified as Preference"
        );
        assert!(sig.negated, "stance-against statement marked negated");
        // Self-match cosine should be ~1.0
        assert!(
            sig.confidence > 0.99,
            "self-match confidence should be ~1.0, got {}",
            sig.confidence
        );
    }

    /// Objective: Verify that empty or whitespace-only utterances produce no
    /// signal without calling the embedder.
    /// Invariants: "" and "   " → 0 signals; embedder call_count unchanged
    /// after the cache build (which already embedded the prototypes).
    #[tokio::test]
    async fn empty_utterance_no_signal() {
        let embedder = StubEmbedder::new();
        let (config, cache) = build_test_cache(&embedder).await;
        // Capture the baseline: build_test_cache already embedded the 3
        // prototypes, so the counter is nonzero before extraction.
        let baseline = embedder.call_count.load(Ordering::SeqCst);
        let extractor = EmbeddingPersonaExtractor::new(&cache, &embedder, config.thresholds);

        let signals = extractor.extract("").await.expect("extract");
        assert!(signals.is_empty(), "empty utterance → no signal");
        let signals = extractor.extract("   ").await.expect("extract");
        assert!(signals.is_empty(), "whitespace utterance → no signal");
        assert_eq!(
            embedder.call_count.load(Ordering::SeqCst),
            baseline,
            "embedder not called for empty utterances"
        );
    }

    /// Objective: Verify is_semantic() returns true for the embedding path.
    /// Invariants: is_semantic() == true.
    #[tokio::test]
    async fn is_semantic_flag() {
        let embedder = StubEmbedder::new();
        let (config, cache) = build_test_cache(&embedder).await;
        let extractor = EmbeddingPersonaExtractor::new(&cache, &embedder, config.thresholds);
        assert!(
            extractor.is_semantic(),
            "embedding extractor is semantic path"
        );
    }

    /// Objective: Verify cosine similarity handles zero-magnitude vectors
    /// gracefully (returns None, skipped in argmax).
    /// Invariants: cosine with zero vector returns None; no panic.
    #[test]
    fn cosine_zero_magnitude_returns_none() {
        let zero: Vec<f32> = vec![0.0, 0.0, 0.0];
        let nonzero = vec![1.0_f32, 2.0, 3.0];
        let result = EmbeddingPersonaExtractor::cosine(&zero, &nonzero);
        assert!(result.is_none(), "zero-magnitude vector → None");
    }

    /// Objective: Verify cosine similarity returns None on dimension mismatch
    /// rather than panicking.
    /// Invariants: cosine([1,2,3], [1,2]) → None.
    #[test]
    fn cosine_dimension_mismatch_returns_none() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![1.0_f32, 2.0];
        let result = EmbeddingPersonaExtractor::cosine(&a, &b);
        assert!(result.is_none(), "dimension mismatch → None");
    }

    /// Objective: Verify identical vectors yield cosine similarity 1.0.
    /// Invariants: cosine(v, v) == 1.0 for nonzero v.
    #[test]
    fn cosine_identical_vectors() {
        let v = vec![1.0_f32, 2.0, 3.0];
        let s = EmbeddingPersonaExtractor::cosine(&v, &v).expect("nonzero vector pair");
        assert!(
            (s - 1.0).abs() < 1e-5,
            "identical vectors → cosine ≈ 1.0, got {s}"
        );
    }

    /// Objective: Verify that dimension mismatch is handled gracefully
    /// (returns None, skipped in argmax) rather than panicking.
    /// Invariants: cosine with mismatched dimensions returns None.
    #[test]
    fn dimension_mismatch_handled_gracefully() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![1.0_f32, 2.0];
        let result = EmbeddingPersonaExtractor::cosine(&a, &b);
        assert!(result.is_none(), "dimension mismatch → None, no panic");
    }
}
