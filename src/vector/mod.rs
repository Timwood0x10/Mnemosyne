//! # Vector Index — abstract nearest-neighbor search.
//!
//! This module defines the [`VectorIndex`] trait and provides two V1
//! implementations:
//!
//! | Implementation | File | Speed | Precision | Lines |
//! |---|---|---|---|---|
//! | `HnswIndex` | `hnsw.rs` | O(log N) | ≈99% | ~200 |
//! | `BruteForceIndex` | `brute_force.rs` | O(N) | 100% | ~100 |
//!
//! BruteForceIndex exists for debugging — when HNSW returns unexpected
//! results, switching to BruteForce reveals whether the issue is in the
//! index or in the embeddings themselves.

mod brute_force;
mod builder;
mod hnsw;

pub use brute_force::BruteForceIndex;
pub use builder::VectorBuilder;
pub use hnsw::HnswIndex;

use crate::error::Error;

/// A nearest-neighbor vector index for entity lookup.
///
/// Implementations must be `Send + Sync` for use behind `Arc` in the MCP server.
pub trait VectorIndex: Send + Sync {
    /// Build the index from a list of `(entity_id, vector)` pairs.
    fn build(&mut self, items: &[(i64, Vec<f32>)]) -> Result<(), Error>;

    /// Search for the `top_k` nearest neighbors of `query`.
    ///
    /// Returns a list of `(entity_id, similarity_score)` sorted by descending
    /// score.
    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(i64, f32)>, Error>;

    /// Human-readable name of this index type (e.g. "hnsw", "brute_force").
    fn name(&self) -> &str;
}
