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
pub use pipeline::{ResolveContext, ResolverPipeline, ResolverStage};
pub use representation::{
    EntityRepresentationBuilder, EntitySnapshot, EventSummary, FixedTemplateBuilder,
};
pub use stats::ResolverStats;

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
