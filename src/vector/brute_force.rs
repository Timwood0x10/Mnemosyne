//! Brute-force vector index — O(N) full scan with cosine similarity.
//!
//! Exists as a ground-truth reference for HNSW debugging. When HNSW returns
//! unexpected results, switching to BruteForce reveals whether the issue is
//! in the index or in the embeddings.

use crate::error::{Error, StorageError};

use super::VectorIndex;

/// Brute-force vector index — compares the query against every stored vector.
pub struct BruteForceIndex {
    items: Vec<(i64, Vec<f32>)>,
}

impl BruteForceIndex {
    /// Create an empty brute-force index.
    pub fn new() -> Self {
        BruteForceIndex { items: Vec::new() }
    }
}

impl Default for BruteForceIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorIndex for BruteForceIndex {
    fn build(&mut self, items: &[(i64, Vec<f32>)]) -> Result<(), Error> {
        // Same boundary as `HnswIndex::validate_items`: zero-dimension vectors
        // and non-finite values are rejected before anything is stored. The
        // brute-force index is documented as HNSW's ground-truth reference, so
        // accepting inputs HNSW rejects made the two disagree on exactly the
        // degenerate cases the reference exists to explain (09-27/M7, M8).
        if let Some((_, first)) = items.first() {
            let dim = first.len();
            if dim == 0 {
                return Err(Error::InvalidInput(
                    "BruteForce vectors must have at least one dimension".to_string(),
                ));
            }
            for (_, v) in items {
                if v.len() != dim {
                    return Err(Error::Storage(StorageError::DimensionMismatch {
                        expected: dim,
                        actual: v.len(),
                    }));
                }
                if v.iter().any(|value| !value.is_finite()) {
                    return Err(Error::InvalidInput(
                        "BruteForce vectors must contain only finite values".to_string(),
                    ));
                }
            }
        }
        self.items = items.to_vec();
        Ok(())
    }

    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(i64, f32)>, Error> {
        if self.items.is_empty() {
            return Ok(Vec::new());
        }

        // Validate the query dimension against the indexed vectors. The old
        // code let `cosine_similarity` silently use `min_len`, so a
        // wrong-dimension query returned plausible-but-wrong scores instead
        // of an error (HNSW already returns DimensionMismatch here; keep the
        // two reference implementations consistent).
        let expected = self.items[0].1.len();
        if query.len() != expected {
            return Err(Error::Storage(StorageError::DimensionMismatch {
                expected,
                actual: query.len(),
            }));
        }
        // Non-finite queries would make `cosine_similarity` produce NaN, and
        // `f32::partial_cmp` reports NaN as unordered against everything, so
        // the ranking below would not be a total order. HNSW rejects them at
        // the same point.
        if query.iter().any(|value| !value.is_finite()) {
            return Err(Error::InvalidInput(
                "BruteForce query must contain only finite values".to_string(),
            ));
        }

        // Compute cosine similarity against every item
        let mut scored: Vec<(i64, f32)> = self
            .items
            .iter()
            .map(|(id, vec)| (*id, cosine_similarity(query, vec)))
            .collect();

        // Sort by descending similarity. `unwrap_or(Ordering::Equal)` keeps the
        // comparison NaN-safe, and the `id` tie-break makes equal scores a
        // deterministic total order (independent of insertion order) instead of
        // leaving their relative rank to the sort implementation.
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(top_k);
        Ok(scored)
    }

    fn name(&self) -> &str {
        "brute_force"
    }
}

/// Compute cosine similarity between two vectors.
///
/// Returns a value in [0, 1] where 1.0 = identical direction.
/// Returns 0.0 if either vector is zero-length.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let min_len = a.len().min(b.len());
    if min_len == 0 {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for i in 0..min_len {
        let ai = a[i] as f64;
        let bi = b[i] as f64;
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        (dot / denom) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec3(x: f32, y: f32, z: f32) -> Vec<f32> {
        vec![x, y, z]
    }

    /// Objective: Verify that identical vectors return similarity ≈ 1.0.
    #[test]
    fn identical_vector_found() {
        let mut index = BruteForceIndex::new();
        index.build(&[(10001, vec3(1.0, 0.0, 0.0))]).unwrap();
        let results = index.search(&vec3(1.0, 0.0, 0.0), 1).unwrap();
        assert_eq!(results.len(), 1, "should return one result");
        assert!(
            results[0].1 > 0.99,
            "identical vector should have near-1.0 cosine"
        );
        assert_eq!(results[0].0, 10001, "should return correct entity ID");
    }

    /// Objective: Verify that orthogonal vectors return score ≈ 0.0.
    #[test]
    fn orthogonal_vector_zero_similarity() {
        let mut index = BruteForceIndex::new();
        index.build(&[(10001, vec3(1.0, 0.0, 0.0))]).unwrap();
        let results = index.search(&vec3(0.0, 1.0, 0.0), 1).unwrap();
        assert!(
            results[0].1.abs() < 0.01,
            "orthogonal vectors should have near-zero similarity"
        );
    }

    /// Objective: Verify that empty index returns empty results.
    #[test]
    fn empty_index_returns_empty() {
        let index = BruteForceIndex::new();
        let results = index.search(&vec3(1.0, 0.0, 0.0), 5).unwrap();
        assert!(
            results.is_empty(),
            "empty index should return empty results"
        );
    }

    /// Objective: Verify that dimension mismatch in build returns an error.
    #[test]
    fn dimension_mismatch_returns_err() {
        let mut index = BruteForceIndex::new();
        let result = index.build(&[(1, vec![1.0, 2.0]), (2, vec![3.0])]);
        assert!(result.is_err(), "dimension mismatch should error");
    }

    /// Objective: Verify correct top-K ordering.
    #[test]
    fn top_k_ordering() {
        let mut index = BruteForceIndex::new();
        index
            .build(&[
                (1, vec3(1.0, 0.0, 0.0)),
                (2, vec3(0.9, 0.1, 0.0)),
                (3, vec3(0.0, 1.0, 0.0)),
            ])
            .unwrap();
        let results = index.search(&vec3(1.0, 0.0, 0.0), 3).unwrap();
        assert_eq!(results.len(), 3, "should return all 3 results");
        assert_eq!(results[0].0, 1, "closest should be first");
        assert_eq!(results[1].0, 2, "second closest should be second");
        assert_eq!(results[2].0, 3, "farthest should be last");
    }

    /// Objective: Verify zero-dimension vectors are rejected at build time, the
    /// same boundary HNSW enforces (09-27/M8).
    /// Invariants: An empty vector returns `InvalidInput`; HNSW returns an error
    /// for the identical input, so the two indexes agree.
    #[test]
    fn zero_dimension_build_rejected() {
        let mut index = BruteForceIndex::new();
        let error = index
            .build(&[(1, Vec::new())])
            .expect_err("an empty (zero-dimension) vector must be rejected");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "zero-dimension input must return a typed InvalidInput error, got {error:?}"
        );
        assert!(
            index.items.is_empty(),
            "a rejected build must not store any items"
        );

        // HNSW rejects the same input, so the two reference implementations agree.
        let mut hnsw = crate::vector::HnswIndex::new();
        assert!(
            hnsw.build(&[(1, Vec::new())]).is_err(),
            "HNSW must reject zero-dimension vectors too, keeping the indexes consistent"
        );
    }

    /// Objective: Verify NaN / infinite component values are rejected at build
    /// time, matching HNSW (09-27/M7).
    /// Invariants: Every non-finite vector returns `InvalidInput` and nothing is stored.
    #[test]
    fn non_finite_build_rejected() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut index = BruteForceIndex::new();
            let error = index
                .build(&[(1, vec![1.0, bad, 0.0])])
                .expect_err("non-finite vector components must be rejected");
            assert!(
                matches!(error, Error::InvalidInput(_)),
                "non-finite component {bad} must return InvalidInput, got {error:?}"
            );
            assert!(
                index.items.is_empty(),
                "a rejected build must not store any items"
            );
        }
    }

    /// Objective: Verify a non-finite query is rejected rather than producing
    /// NaN scores with an unordered ranking (09-27/M7).
    /// Invariants: A NaN query returns `InvalidInput`; HNSW agrees on the same input.
    #[test]
    fn non_finite_query_rejected() {
        let items = [(1, vec3(1.0, 0.0, 0.0))];
        let mut index = BruteForceIndex::new();
        index.build(&items).unwrap();
        let error = index
            .search(&vec3(f32::NAN, 0.0, 0.0), 1)
            .expect_err("a NaN query must be rejected");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "NaN query must return InvalidInput, got {error:?}"
        );

        let mut hnsw = crate::vector::HnswIndex::new();
        hnsw.build(&items).unwrap();
        assert!(
            hnsw.search(&vec3(f32::NAN, 0.0, 0.0), 1).is_err(),
            "HNSW must reject the same NaN query, keeping the indexes consistent"
        );
    }

    /// Objective: Verify search ordering is a deterministic total order when
    /// scores tie, independent of insertion order.
    /// Invariants: Tied scores are ordered by ascending entity id, and reversing
    /// the build order yields the identical ranking.
    #[test]
    fn tied_scores_ordering_is_deterministic() {
        let ascending = [
            (3, vec3(1.0, 0.0, 0.0)),
            (5, vec3(1.0, 0.0, 0.0)),
            (9, vec3(1.0, 0.0, 0.0)),
        ];
        let descending = [
            (9, vec3(1.0, 0.0, 0.0)),
            (5, vec3(1.0, 0.0, 0.0)),
            (3, vec3(1.0, 0.0, 0.0)),
        ];

        let mut a = BruteForceIndex::new();
        a.build(&ascending).unwrap();
        let mut b = BruteForceIndex::new();
        b.build(&descending).unwrap();

        let query = vec3(1.0, 0.0, 0.0);
        let first = a.search(&query, 3).unwrap();
        let second = b.search(&query, 3).unwrap();
        assert_eq!(
            first, second,
            "reverse insertion order must produce the identical ranking for tied scores"
        );
        let ids: Vec<i64> = first.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            vec![3, 5, 9],
            "tied scores must be ordered by ascending id, got {ids:?}"
        );
    }
}
