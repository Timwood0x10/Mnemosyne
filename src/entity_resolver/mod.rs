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
    /// Records stats for monitoring (alias hit / unknown / total mentions).
    pub fn resolve(&self, mention: &str) -> ResolveResult {
        self.stats.record_mention();
        let result = self.pipeline.resolve(mention);
        match &result {
            ResolveResult::Matched { .. } => self.stats.record_alias_hit(),
            ResolveResult::Unknown { .. } => self.stats.record_unknown(),
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
