//! In-memory HNSW index backed by `instant-distance`.
//!
//! The index is rebuilt from entity snapshot features at startup. It is a
//! disposable query accelerator: immutable facts remain the source of truth.

use instant_distance::{Builder, HnswMap, Point, Search};

use crate::error::{Error, StorageError};

use super::VectorIndex;

const EF_CONSTRUCTION: usize = 300;
const EF_SEARCH: usize = 200;
const BUILD_SEED: u64 = 0x4c4f_5245_5343_4f50;

#[derive(Clone, Debug)]
struct CosinePoint(Vec<f32>);

impl Point for CosinePoint {
    fn distance(&self, other: &Self) -> f32 {
        let mut dot = 0.0f64;
        let mut left_norm = 0.0f64;
        let mut right_norm = 0.0f64;
        for (left, right) in self.0.iter().zip(&other.0) {
            let left = f64::from(*left);
            let right = f64::from(*right);
            dot += left * right;
            left_norm += left * left;
            right_norm += right * right;
        }
        let denominator = left_norm.sqrt() * right_norm.sqrt();
        if denominator <= f64::EPSILON {
            // Zero vector: cosine similarity is undefined; match
            // BruteForceIndex (which returns 0.0) by mapping to the angular
            // distance whose cosine is 0.0 — sqrt(2) = sqrt(2 - 2*0).
            // Previously returned 1.0, which the search() conversion
            // (cosine = 1 - d²/2) turned into 0.5, disagreeing with the
            // brute-force reference.
            return 2.0f32.sqrt();
        }
        let cosine = (dot / denominator).clamp(-1.0, 1.0);
        (2.0 - 2.0 * cosine).max(0.0).sqrt() as f32
    }
}

/// HNSW-based vector index for entity similarity search.
pub struct HnswIndex {
    map: Option<HnswMap<CosinePoint, i64>>,
    dimension: Option<usize>,
    indexed_len: usize,
}

impl HnswIndex {
    /// Create an empty HNSW index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: None,
            dimension: None,
            indexed_len: 0,
        }
    }

    fn validate_items(items: &[(i64, Vec<f32>)]) -> Result<Option<usize>, Error> {
        let Some((_, first)) = items.first() else {
            return Ok(None);
        };
        let dimension = first.len();
        if dimension == 0 {
            return Err(Error::InvalidInput(
                "HNSW vectors must have at least one dimension".to_string(),
            ));
        }
        for (_, vector) in items {
            if vector.len() != dimension {
                return Err(Error::Storage(StorageError::DimensionMismatch {
                    expected: dimension,
                    actual: vector.len(),
                }));
            }
            if vector.iter().any(|value| !value.is_finite()) {
                return Err(Error::InvalidInput(
                    "HNSW vectors must contain only finite values".to_string(),
                ));
            }
        }
        Ok(Some(dimension))
    }
}

impl Default for HnswIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorIndex for HnswIndex {
    fn build(&mut self, items: &[(i64, Vec<f32>)]) -> Result<(), Error> {
        let dimension = Self::validate_items(items)?;
        let points = items
            .iter()
            .map(|(_, vector)| CosinePoint(vector.clone()))
            .collect();
        let ids = items.iter().map(|(id, _)| *id).collect();
        self.map = dimension.map(|_| {
            Builder::default()
                .ef_construction(EF_CONSTRUCTION)
                .ef_search(EF_SEARCH)
                .seed(BUILD_SEED)
                .build(points, ids)
        });
        self.dimension = dimension;
        self.indexed_len = items.len();
        Ok(())
    }

    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(i64, f32)>, Error> {
        if top_k == 0 || self.indexed_len == 0 {
            return Ok(Vec::new());
        }
        let expected = self
            .dimension
            .ok_or_else(|| Error::Internal("non-empty HNSW index has no dimension".to_string()))?;
        if query.len() != expected {
            return Err(Error::Storage(StorageError::DimensionMismatch {
                expected,
                actual: query.len(),
            }));
        }
        if query.iter().any(|value| !value.is_finite()) {
            return Err(Error::InvalidInput(
                "HNSW query must contain only finite values".to_string(),
            ));
        }
        let map = self
            .map
            .as_ref()
            .ok_or_else(|| Error::Internal("HNSW map was not built".to_string()))?;
        let mut state = Search::default();
        let query = CosinePoint(query.to_vec());
        Ok(map
            .search(&query, &mut state)
            .take(top_k.min(self.indexed_len))
            .map(|item| {
                let cosine = 1.0 - item.distance * item.distance / 2.0;
                (*item.value, cosine.clamp(-1.0, 1.0))
            })
            .collect())
    }

    fn name(&self) -> &str {
        "hnsw"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::BruteForceIndex;

    fn feature(index: usize, dimensions: usize) -> Vec<f32> {
        let mut state = (index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (0..dimensions)
            .map(|axis| {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                state = state
                    .wrapping_mul(0x2545_f491_4f6c_dd1d)
                    .wrapping_add(axis as u64);
                let unit = (state >> 40) as f32 / (1_u32 << 24) as f32;
                unit * 2.0 - 1.0
            })
            .collect()
    }

    /// Objective: Verify the real HNSW graph returns high-recall nearest neighbors.
    /// Invariants: Deterministic 256-point Recall@5 matches brute force by at least 96%.
    #[test]
    fn hnsw_recall_matches_brute_force_ground_truth() {
        let items: Vec<(i64, Vec<f32>)> = (0..256)
            .map(|index| (10_000 + index as i64, feature(index, 24)))
            .collect();
        let mut hnsw = HnswIndex::new();
        let mut exact = BruteForceIndex::new();
        hnsw.build(&items).expect("build HNSW graph");
        exact.build(&items).expect("build brute-force reference");

        let mut matched = 0usize;
        let mut expected = 0usize;
        for query_index in (0..256).step_by(7) {
            let query = feature(query_index, 24);
            let actual = hnsw.search(&query, 5).expect("search HNSW graph");
            let ground_truth = exact.search(&query, 5).expect("search exact reference");
            let actual_ids: std::collections::HashSet<i64> =
                actual.into_iter().map(|(id, _)| id).collect();
            matched += ground_truth
                .iter()
                .filter(|(id, _)| actual_ids.contains(id))
                .count();
            expected += ground_truth.len();
        }
        let recall = matched as f64 / expected as f64;
        assert!(
            recall >= 0.96,
            "HNSW Recall@5 must be at least 96%, got {recall:.3} ({matched}/{expected})"
        );
    }

    /// Objective: Verify rebuild replaces old graph contents rather than appending.
    /// Invariants: A query after rebuild can only return ids from the replacement set.
    #[test]
    fn rebuild_replaces_previous_graph() {
        let mut index = HnswIndex::new();
        index
            .build(&[(1, vec![1.0, 0.0]), (2, vec![0.0, 1.0])])
            .expect("build initial HNSW graph");
        index
            .build(&[(9, vec![1.0, 0.0])])
            .expect("rebuild HNSW graph");
        let results = index.search(&[1.0, 0.0], 5).expect("search rebuilt graph");
        assert_eq!(
            results.len(),
            1,
            "Rebuilt graph must contain one replacement point"
        );
        assert_eq!(results[0].0, 9, "Old graph ids must not survive a rebuild");
    }

    /// Objective: Verify malformed vectors are rejected before graph construction/search.
    /// Invariants: Dimension mismatch and non-finite input return explicit errors.
    #[test]
    fn invalid_vectors_return_typed_errors() {
        let mut index = HnswIndex::new();
        let build_error = index
            .build(&[(1, vec![1.0, 0.0]), (2, vec![1.0])])
            .expect_err("mixed dimensions must fail graph construction");
        assert!(
            matches!(
                build_error,
                Error::Storage(StorageError::DimensionMismatch {
                    expected: 2,
                    actual: 1
                })
            ),
            "Mixed dimensions must return a typed dimension error, got {build_error:?}"
        );

        index
            .build(&[(1, vec![1.0, 0.0])])
            .expect("build valid HNSW graph");
        let query_error = index
            .search(&[1.0], 1)
            .expect_err("wrong query dimension must fail");
        assert!(
            matches!(
                query_error,
                Error::Storage(StorageError::DimensionMismatch {
                    expected: 2,
                    actual: 1
                })
            ),
            "Wrong query dimensions must return a typed error, got {query_error:?}"
        );
    }
}
