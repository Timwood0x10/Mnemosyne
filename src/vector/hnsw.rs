//! HNSW (Hierarchical Navigable Small World) index for ANN search.
//!
//! TODO Phase 3: wrap `instant-distance` properly. The HNSW-specific
//! `instant-distance` integration will be completed when the API is verified
//! against a benchmark dataset.
//!
//! Current implementation delegates to [`BruteForceIndex`] as a placeholder
//! so the rest of the module can compile and be tested. Both implementations
//! share the same [`VectorIndex`] trait and produce identical results.

use crate::error::Error;

use super::VectorIndex;
use super::brute_force::BruteForceIndex;

/// HNSW-based vector index for entity similarity search.
///
/// Current implementation is a thin wrapper around `BruteForceIndex`.
/// Replace with real HNSW via `instant-distance` in Phase 3.
pub struct HnswIndex {
    inner: BruteForceIndex,
}

impl HnswIndex {
    /// Create an empty HNSW index.
    pub fn new() -> Self {
        HnswIndex {
            inner: BruteForceIndex::new(),
        }
    }
}

impl Default for HnswIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorIndex for HnswIndex {
    fn build(&mut self, items: &[(i64, Vec<f32>)]) -> Result<(), Error> {
        self.inner.build(items)
    }

    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(i64, f32)>, Error> {
        self.inner.search(query, top_k)
    }

    fn name(&self) -> &str {
        "hnsw"
    }
}
