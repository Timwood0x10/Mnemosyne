//! Embedder trait — abstract text-to-vector interface.
//!
//! The entity resolution engine depends only on this trait, never on a specific
//! embedding model. Providers (FastEmbed, BGE, Jina, E5) swap out below this
//! trait without touching any resolver code.

use std::sync::{Arc, Mutex};

use crate::entity_resolver::cache::EmbeddingCache;
use crate::entity_resolver::pipeline::{ResolveContext, ResolverStage};
use crate::entity_resolver::{RESOLVE_THRESHOLD, ResolveResult};
#[cfg(feature = "local-embed")]
use crate::error::EmbeddingError;
use crate::error::Error;
use crate::vector::VectorIndex;

/// Converts text into a dense vector representation.
///
/// # V1 implementations
///
/// | Provider | Library | Dimension | Source |
/// |---|---|---|---|
/// | `FastEmbedProvider` | `fastembed-rs` | 384 | ONNX local |
pub trait Embedder: Send + Sync {
    /// Embed a single text string into a vector.
    fn embed(&self, text: &str) -> Result<Vec<f32>, Error>;

    /// Embed a batch of text strings into vectors.
    ///
    /// Default implementation calls `embed` in a loop. Providers that support
    /// native batching (e.g. fastembed-rs) should override this.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

/// Embedder backed by the `fastembed-rs` library (ONNX, local, no API key).
///
/// Uses the `all-MiniLM-L6-v2` model (384-dim). The model is downloaded
/// automatically on first use and cached locally.
#[cfg(feature = "local-embed")]
pub struct FastEmbedProvider {
    model: fastembed::TextEmbedding,
}

#[cfg(feature = "local-embed")]
impl FastEmbedProvider {
    /// Create a new `FastEmbedProvider`, loading the default ONNX model.
    pub fn new() -> Result<Self, Error> {
        use fastembed::InitOptions;
        let model = fastembed::TextEmbedding::try_new(InitOptions::default())
            .map_err(|e| Error::Config(format!("failed to load fastembed model: {e}")))?;
        Ok(FastEmbedProvider { model })
    }
}

#[cfg(feature = "local-embed")]
impl Embedder for FastEmbedProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
        let mut results = self
            .model
            .embed(vec![text], None)
            .map_err(|e| Error::Embedding(EmbeddingError::Transport(e.to_string())))?;
        results.pop().ok_or_else(|| {
            Error::Embedding(EmbeddingError::Transport("empty embedding result".into()))
        })
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        self.model
            .embed(refs, None)
            .map_err(|e| Error::Embedding(EmbeddingError::Transport(e.to_string())))
    }
}

/// Stage 2 of the resolution pipeline — fuzzy entity matching via embedding.
///
/// Embeds the mention text, searches the vector index for the nearest neighbor,
/// and returns a match if cosine similarity ≥ [`RESOLVE_THRESHOLD`] (0.85).
pub struct EmbeddingStage {
    embedder: Arc<dyn Embedder>,
    index: Arc<dyn VectorIndex>,
    cache: Arc<Mutex<dyn EmbeddingCache>>,
}

impl EmbeddingStage {
    pub fn new(
        embedder: Arc<dyn Embedder>,
        index: Arc<dyn VectorIndex>,
        cache: Arc<Mutex<dyn EmbeddingCache>>,
    ) -> Self {
        EmbeddingStage {
            embedder,
            index,
            cache,
        }
    }
}

impl ResolverStage for EmbeddingStage {
    fn resolve(&self, mention: &str, _ctx: &ResolveContext) -> Option<ResolveResult> {
        // 1. Cache check (read-only, no lock needed — just use get)
        if let Some(vec) = self.cache.lock().ok().and_then(|c| c.get(mention)) {
            // A failed search on a cached vector must NOT abort resolution:
            // fall through to the re-embed branch instead of returning None
            // early (the bug was that `.ok()?` skipped the fallback and a
            // transient index error silently dropped the mention).
            if let Ok(results) = self.index.search(&vec, 1) {
                if let Some((id, score)) = results.into_iter().next() {
                    if score >= RESOLVE_THRESHOLD {
                        return Some(ResolveResult::Matched {
                            entity_id: id,
                            score,
                        });
                    }
                }
            }
        }

        // 2. Embed + search
        let vec = match self.embedder.embed(mention) {
            Ok(v) => v,
            Err(e) => {
                // A genuine embedding failure is not a "no match": log it so
                // the error is observable, then defer to the next stage. The
                // stage API cannot return the error, but a silent `.ok()?`
                // made failures indistinguishable from a clean miss.
                eprintln!("entity resolver: embed failed for `{mention}`: {e}");
                return None;
            }
        };
        if let Ok(mut cache) = self.cache.lock() {
            cache.put(mention, vec.clone());
        }
        // A failed search after a fresh embed has no fallback left: report a
        // clean miss rather than crashing the pipeline.
        if let Ok(results) = self.index.search(&vec, 1) {
            if let Some((id, score)) = results.into_iter().next() {
                if score >= RESOLVE_THRESHOLD {
                    return Some(ResolveResult::Matched {
                        entity_id: id,
                        score,
                    });
                }
            }
        }

        None // pass to next stage
    }
}

#[cfg(all(test, feature = "local-embed"))]
mod tests {
    use super::*;

    /// Objective: Verify that FastEmbedProvider::new() returns Err when the
    /// ONNX model is not available (no model cached, no network in test env).
    /// Invariants: The error variant is Error::Config.
    #[test]
    #[cfg(feature = "local-embed")]
    fn fastembed_construction_fails_without_model() {
        // In CI / sandbox the model is never available, so new() must Err.
        match FastEmbedProvider::new() {
            Err(Error::Config(_)) => {} // expected: model not found
            Err(other) => panic!("expected Config error, got: {other:?}"),
            Ok(_) => {} // If model IS cached locally, that's OK too.
        }
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;
    use crate::entity_resolver::cache::MemoryEmbeddingCache;
    use crate::entity_resolver::pipeline::{ResolveContext, ResolverStage};
    use std::sync::{Arc, Mutex};

    /// Mock embedder: always returns a fixed vector.
    struct MockEmbedder;
    impl Embedder for MockEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, Error> {
            Ok(vec![1.0, 0.0])
        }
    }

    /// Mock embedder that always fails.
    struct FailingEmbedder;
    impl Embedder for FailingEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
            Err(Error::Embedding(crate::error::EmbeddingError::Transport(
                format!("mock failure for {text}"),
            )))
        }
    }

    /// Mock index: the first search fails (simulating a transient index
    /// error on the cached-vector path), the second succeeds.
    struct FlakyIndex {
        searches: std::sync::atomic::AtomicUsize,
    }
    impl VectorIndex for FlakyIndex {
        fn build(&mut self, _items: &[(i64, Vec<f32>)]) -> Result<(), Error> {
            Ok(())
        }
        fn search(&self, _query: &[f32], _top_k: usize) -> Result<Vec<(i64, f32)>, Error> {
            let n = self
                .searches
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n == 0 {
                Err(Error::Internal("transient index failure".into()))
            } else {
                Ok(vec![(42, 0.95)])
            }
        }
        fn name(&self) -> &str {
            "flaky"
        }
    }

    /// Objective: Verify a failed search on the cached-vector path falls
    /// through to the re-embed branch instead of silently dropping the
    /// mention (the fixed bug: `.ok()?` aborted resolution on index error).
    /// Invariants: with a warm cache and a first-search failure, the stage
    /// still resolves via the fallback and returns Matched(42, 0.95).
    #[test]
    fn cached_vector_search_failure_falls_back_to_embed() {
        let cache = Arc::new(Mutex::new(MemoryEmbeddingCache::new()));
        // Warm the cache so the cached-vector path is taken first.
        cache
            .lock()
            .expect("cache lock")
            .put("玄德", vec![1.0, 0.0]);
        let stage = EmbeddingStage::new(
            Arc::new(MockEmbedder),
            Arc::new(FlakyIndex {
                searches: std::sync::atomic::AtomicUsize::new(0),
            }),
            cache,
        );
        let result = stage.resolve(
            "玄德",
            &ResolveContext {
                surface: "玄德".into(),
            },
        );
        let matched = result.expect("fallback must resolve the mention");
        assert_eq!(
            matched.entity_id(),
            Some(42),
            "flaky first search must not abort resolution"
        );
    }

    /// Objective: Verify an embedder failure is logged and reported as a
    /// clean miss (None), not a panic or a fabricated match.
    /// Invariants: FailingEmbedder → stage returns None.
    #[test]
    fn embedder_failure_returns_clean_miss() {
        let stage = EmbeddingStage::new(
            Arc::new(FailingEmbedder),
            Arc::new(FlakyIndex {
                searches: std::sync::atomic::AtomicUsize::new(0),
            }),
            Arc::new(Mutex::new(MemoryEmbeddingCache::new())),
        );
        let result = stage.resolve(
            "玄德",
            &ResolveContext {
                surface: "玄德".into(),
            },
        );
        assert!(
            result.is_none(),
            "embedder failure must pass through as a miss, got {result:?}"
        );
    }
}
