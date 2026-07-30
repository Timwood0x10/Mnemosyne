//! Brute-force vector index — O(N) full scan with cosine similarity.
//!
//! Exists as a ground-truth reference for HNSW debugging. When HNSW returns
//! unexpected results, switching to BruteForce reveals whether the issue is
//! in the index or in the embeddings.

use crate::error::Error;

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
        // Validate dimensions are consistent
        if let Some((_, first)) = items.first() {
            let dim = first.len();
            for (_, v) in items {
                if v.len() != dim {
                    return Err(Error::InvalidInput(format!(
                        "dimension mismatch: expected {}, got {}",
                        dim,
                        v.len()
                    )));
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

        // Compute cosine similarity against every item
        let mut scored: Vec<(i64, f32)> = self
            .items
            .iter()
            .map(|(id, vec)| (*id, cosine_similarity(query, vec)))
            .collect();

        // Sort by descending similarity
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
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
}
