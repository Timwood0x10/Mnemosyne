//! # Entity Resolution Engine
//!
//! Resolves text mentions (e.g. "玄德", "刘皇叔", "云长") to canonical entity IDs
//! through a chain of resolver stages — exact alias matching first, then
//! embedding-based fuzzy matching as a fallback.
//!
//! ## Pipeline
//!
//! ```text
//! Mention
//!   │
//!   ├── Stage 1: AliasResolver  (HashMap exact match, O(1))
//!   │   ├── hit  → Matched
//!   │   └── miss → next stage
//!   │
//!   ├── Stage 2: EmbeddingResolver
//!   │   ├── Cache check → EmbeddingCache
//!   │   ├── Embedder    → cosine similarity
//!   │   └── Cosine ≥ threshold → Matched / Unknown
//!   │
//!   └── Stage 3+ (V2): RegexResolver / PronounResolver / TitleResolver
//! ```

mod alias;
mod cache;
mod embedding;
mod pipeline;
mod representation;
mod stats;

pub use alias::AliasResolver;
pub use alias::AliasStage;
pub use cache::{EmbeddingCache, MemoryEmbeddingCache};
pub use embedding::Embedder;
pub use embedding::EmbeddingStage;
#[cfg(feature = "local-embed")]
pub use embedding::FastEmbedProvider;
pub use pipeline::{ResolveContext, ResolverPipeline, ResolverStage};
pub use representation::{
    EntityRepresentationBuilder, EntitySnapshot, EventSummary, FixedTemplateBuilder,
};
pub use stats::ResolverStats;

use std::sync::{Arc, Mutex};

use crate::vector::VectorIndex;

/// Cosine similarity threshold for fuzzy entity resolution.
///
/// Matches with cosine ≥ THRESHOLD are considered resolved; those below
/// are treated as `Unknown`. The value 0.85 was established empirically
/// during entity-resolver benchmarks on the Four Great Classical Novels.
pub const RESOLVE_THRESHOLD: f32 = 0.85;

/// Top-level entity resolver — builds the pipeline, runs stages, records stats.
///
/// # Example
///
/// ```ignore
/// let aliases = AliasResolver::from_pairs([("玄德".into(), 10001)]);
/// let resolver = EntityResolver::new(aliases);
/// let result = resolver.resolve("玄德");
/// assert!(result.is_matched());
/// resolver.stats().print_report();
/// ```
pub struct EntityResolver {
    pipeline: ResolverPipeline,
    stats: Arc<ResolverStats>,
}

impl EntityResolver {
    /// Build a resolver with an alias stage backed by `aliases`.
    ///
    /// V1 always includes the alias stage. Embedding stage will be added in V2
    /// when a concrete `Embedder` implementation is wired in.
    pub fn new(aliases: AliasResolver) -> Self {
        let mut pipeline = ResolverPipeline::empty();
        pipeline.push(AliasStage::new(aliases));
        EntityResolver {
            pipeline,
            stats: Arc::new(ResolverStats::new()),
        }
    }

    /// Add an embedding stage to the pipeline (after alias, before fallback).
    ///
    /// The embedding stage performs fuzzy matching: mention → embed → vector
    /// search → threshold check. When the alias stage fails to match, the
    /// embedding stage may still resolve the mention via cosine similarity.
    pub fn with_embedding(
        mut self,
        embedder: Arc<dyn Embedder>,
        index: Arc<dyn VectorIndex>,
        cache: Arc<Mutex<dyn EmbeddingCache>>,
    ) -> Self {
        self.pipeline
            .push(EmbeddingStage::new(embedder, index, cache));
        self
    }

    /// Resolve a mention to an entity.
    ///
    /// Records stats for monitoring. The winning pipeline stage determines
    /// the counter: stage 0 is the alias stage (`EntityResolver::new` always
    /// pushes it first), any later stage (embedding) records an embedding
    /// hit — previously every `Matched` was counted as an alias hit, so
    /// `embedding_hit_rate` stayed 0 forever.
    pub fn resolve(&self, mention: &str) -> ResolveResult {
        self.stats.record_mention();
        let (result, stage_idx) = self.pipeline.resolve_with_stage(mention);
        match (&result, stage_idx) {
            // Stage 0 is the alias stage (always pushed first by `new`).
            (ResolveResult::Matched { .. }, Some(0)) => self.stats.record_alias_hit(),
            // Any later stage (embedding), plus the impossible `Matched, None`
            // combination the compiler cannot rule out.
            (ResolveResult::Matched { .. }, _) => self.stats.record_embedding_hit(),
            (ResolveResult::Unknown { .. }, _) => self.stats.record_unknown(),
        }
        result
    }

    /// Reference to the resolver statistics collector.
    pub fn stats(&self) -> &Arc<ResolverStats> {
        &self.stats
    }

    /// Reset all statistics counters.
    pub fn reset_stats(&self) {
        self.stats.reset();
    }
}

/// Result of resolving a mention to an entity.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolveResult {
    /// The mention was successfully resolved to a known entity.
    Matched { entity_id: i64, score: f32 },
    /// The mention could not be resolved to any known entity.
    Unknown { surface: String },
}

impl ResolveResult {
    /// Returns `true` if the result is a match.
    pub fn is_matched(&self) -> bool {
        matches!(self, ResolveResult::Matched { .. })
    }

    /// Returns the entity ID if matched, `None` otherwise.
    pub fn entity_id(&self) -> Option<i64> {
        match self {
            ResolveResult::Matched { entity_id, .. } => Some(*entity_id),
            ResolveResult::Unknown { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_resolver::alias::AliasResolver;
    use crate::entity_resolver::cache::MemoryEmbeddingCache;
    use crate::entity_resolver::embedding::Embedder;

    /// Deterministic embedder for tests — every mention maps to one vector.
    struct MockEmbedder;
    impl Embedder for MockEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, crate::error::Error> {
            Ok(vec![1.0, 0.0, 0.0])
        }
    }

    /// Deterministic index: any query resolves to a fixed entity above the
    /// RESOLVE_THRESHOLD (0.85).
    struct MockIndex;
    impl crate::vector::VectorIndex for MockIndex {
        fn build(&mut self, _items: &[(i64, Vec<f32>)]) -> Result<(), crate::error::Error> {
            Ok(())
        }
        fn search(
            &self,
            _query: &[f32],
            _top_k: usize,
        ) -> Result<Vec<(i64, f32)>, crate::error::Error> {
            Ok(vec![(10001, 0.95)])
        }
        fn name(&self) -> &str {
            "mock"
        }
    }

    fn resolver_with_embedding() -> EntityResolver {
        let aliases = AliasResolver::from_pairs([("玄德".into(), 1)]);
        EntityResolver::new(aliases).with_embedding(
            Arc::new(MockEmbedder),
            Arc::new(MockIndex),
            Arc::new(std::sync::Mutex::new(MemoryEmbeddingCache::new())),
        )
    }

    /// Objective: Verify an alias-stage match is counted as an alias hit.
    /// Invariants: after resolving a known alias, alias_hit_rate > 0 and
    /// embedding_hit_rate == 0.
    #[test]
    fn alias_hit_counts_as_alias() {
        let resolver = resolver_with_embedding();
        let result = resolver.resolve("玄德");
        assert!(result.is_matched(), "玄德 must resolve via alias stage");
        assert!(
            resolver.stats().alias_hit_rate() > 0.0,
            "alias hit must be recorded, got rate {}",
            resolver.stats().alias_hit_rate()
        );
        assert_eq!(
            resolver.stats().embedding_hit_rate(),
            0.0,
            "alias hit must NOT be counted as an embedding hit"
        );
    }

    /// Objective: Verify an embedding-stage match (alias miss) is counted as
    /// an embedding hit — the previously-dead stat path.
    /// Invariants: after resolving an unknown-to-alias mention, the mock
    /// index matches it, so embedding_hit_rate > 0.
    #[test]
    fn embedding_hit_counts_as_embedding() {
        let resolver = resolver_with_embedding();
        // 刘皇叔 is not in the alias map; the mock embedding stage resolves it.
        let result = resolver.resolve("刘皇叔");
        assert!(
            result.is_matched(),
            "刘皇叔 must resolve via embedding stage"
        );
        assert_eq!(
            result.entity_id(),
            Some(10001),
            "mock index returns entity 10001"
        );
        assert!(
            resolver.stats().embedding_hit_rate() > 0.0,
            "embedding hit must be recorded, got rate {}",
            resolver.stats().embedding_hit_rate()
        );
    }

    /// Objective: Verify Unknown is reachable when the embedding stage misses.
    /// Invariants: with a rejecting index, an alias-miss mention yields
    /// Unknown and unknown_rate > 0.
    #[test]
    fn unknown_reachable_when_embedding_misses() {
        struct RejectingIndex;
        impl crate::vector::VectorIndex for RejectingIndex {
            fn build(&mut self, _: &[(i64, Vec<f32>)]) -> Result<(), crate::error::Error> {
                Ok(())
            }
            fn search(&self, _: &[f32], _: usize) -> Result<Vec<(i64, f32)>, crate::error::Error> {
                Ok(vec![(10001, 0.1)]) // below RESOLVE_THRESHOLD
            }
            fn name(&self) -> &str {
                "rejecting"
            }
        }
        let aliases = AliasResolver::from_pairs([("玄德".into(), 1)]);
        let resolver = EntityResolver::new(aliases).with_embedding(
            Arc::new(MockEmbedder),
            Arc::new(RejectingIndex),
            Arc::new(std::sync::Mutex::new(MemoryEmbeddingCache::new())),
        );
        let result = resolver.resolve("刘皇叔");
        assert!(!result.is_matched(), "below-threshold embedding → Unknown");
        assert!(
            resolver.stats().unknown_rate() > 0.0,
            "unknown must be recorded, got rate {}",
            resolver.stats().unknown_rate()
        );
    }
}
