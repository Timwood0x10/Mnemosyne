//! Centroid memory compression (pure embedding, no LLM).
//!
//! Implements the plan's P1: repeated utterances about the same topic/emotion
//! are compressed into a **representative memory** by clustering their
//! embeddings and computing the geometric centroid:
//!
//! ```text
//! C = (1/n) · Σ Emb(Sᵢ)
//! ```
//!
//! Compression is lossy-but-bounded: the centroid preserves the semantic
//! direction and discards per-utterance detail. Retrieval uses the centroid
//! vector; **injection always uses the representative original sentences**
//! (never an LLM rewrite) — "检索代数化、注入原文化", no hallucination.

use crate::anchor::mean_vector;
use crate::resolver::cosine_similarity;

/// Maximum representative original sentences kept per cluster (injection
/// fidelity cap).
pub const MAX_REPRESENTATIVES: usize = 3;

/// A compressed cluster: centroid for retrieval + original texts for
/// injection + repetition count.
#[derive(Debug, Clone, PartialEq)]
pub struct CentroidGroup {
    /// Geometric center of the cluster's embeddings.
    pub centroid: Vec<f32>,
    /// Representative original sentences (≤ [`MAX_REPRESENTATIVES`]).
    pub representatives: Vec<String>,
    /// How many utterances this cluster absorbed.
    pub count: usize,
}

/// Greedy clustering + centroid reduction over an anchor-bucket's utterances.
#[derive(Debug, Clone, Default)]
pub struct CentroidCompressor;

impl CentroidCompressor {
    /// Reduce a set of (vector, text) pairs into centroid clusters.
    ///
    /// Greedy: each utterance joins the first cluster whose centroid is
    /// within `eps` cosine-distance (1 − cos ≥ eps ⇒ join); otherwise it
    /// starts a new cluster. Cluster centroids are recomputed as they grow.
    /// Returns an empty vec when inputs are empty or all vectors unusable.
    #[must_use]
    pub fn reduce(vectors: &[Vec<f32>], texts: &[String], eps: f64) -> Vec<CentroidGroup> {
        if vectors.is_empty() || vectors.len() != texts.len() {
            return Vec::new();
        }
        let mut groups: Vec<Vec<(usize, Vec<f32>)>> = Vec::new(); // cluster → (text idx, vec)
        for (i, v) in vectors.iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            let mut placed = false;
            for group in &mut groups {
                let centroid =
                    mean_vector(&group.iter().map(|(_, vec)| vec.clone()).collect::<Vec<_>>());
                if let Some(c) = centroid {
                    if let Some(sim) = cosine_similarity(v, &c) {
                        if (1.0 - sim) <= eps {
                            group.push((i, v.clone()));
                            placed = true;
                            break;
                        }
                    }
                }
            }
            if !placed {
                groups.push(vec![(i, v.clone())]);
            }
        }

        groups
            .into_iter()
            .filter_map(|group| {
                let centroid =
                    mean_vector(&group.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>())?;
                // Keep the NEWEST representatives (group is insertion-
                // ordered = earliest utterances): after a stance flip the
                // first three members still quoted the pre-flip wording
                // while the latest (current) stance was dropped.
                let mut representatives: Vec<String> = group
                    .iter()
                    .rev()
                    .take(MAX_REPRESENTATIVES)
                    .map(|(idx, _)| texts[*idx].clone())
                    .collect();
                representatives.reverse();
                representatives.truncate(MAX_REPRESENTATIVES);
                Some(CentroidGroup {
                    centroid,
                    representatives,
                    count: group.len(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txts(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("sentence {i}")).collect()
    }

    /// Objective: Verify identical-direction vectors compress into ONE
    /// cluster whose centroid equals their mean.
    /// Invariants: three same-direction vectors → one group, count=3,
    /// centroid == mean, representatives ≤ 3.
    #[test]
    fn identical_vectors_compress() {
        let vectors = vec![vec![1.0_f32, 0.0], vec![2.0_f32, 0.0], vec![3.0_f32, 0.0]];
        let texts = txts(3);
        let groups = CentroidCompressor::reduce(&vectors, &texts, 0.2);

        assert_eq!(groups.len(), 1, "same-direction vectors must cluster");
        assert_eq!(groups[0].count, 3, "cluster absorbs all three utterances");
        assert_eq!(
            groups[0].centroid,
            vec![2.0_f32, 0.0],
            "centroid is the mean"
        );
        assert!(
            groups[0].representatives.len() <= MAX_REPRESENTATIVES,
            "representatives capped"
        );
    }

    /// Objective: Verify orthogonal vectors do NOT cluster.
    /// Invariants: perpendicular directions → separate clusters.
    #[test]
    fn far_vectors_do_not_cluster() {
        let vectors = vec![vec![1.0_f32, 0.0], vec![0.0_f32, 1.0]];
        let texts = txts(2);
        let groups = CentroidCompressor::reduce(&vectors, &texts, 0.2);
        assert_eq!(groups.len(), 2, "orthogonal vectors must stay separate");
        assert_eq!(groups[0].count, 1);
        assert_eq!(groups[1].count, 1);
    }

    /// Objective: Verify representatives never exceed the cap even with many
    /// utterances in one cluster.
    /// Invariants: 10 identical utterances → 1 group, representatives == 3,
    /// count == 10.
    #[test]
    fn representatives_capped() {
        let vectors = vec![vec![1.0_f32, 0.0]; 10];
        let texts = txts(10);
        let groups = CentroidCompressor::reduce(&vectors, &texts, 0.2);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 10, "all ten utterances absorbed");
        assert_eq!(
            groups[0].representatives.len(),
            MAX_REPRESENTATIVES,
            "representatives hard-capped at 3"
        );
    }

    /// Objective: Verify empty / mismatched inputs degrade gracefully.
    /// Invariants: no vectors, or vectors/тексты length mismatch → empty.
    #[test]
    fn empty_or_mismatched_input_returns_empty() {
        assert!(CentroidCompressor::reduce(&[], &[], 0.2).is_empty());
        let v = vec![vec![1.0_f32, 0.0]];
        let t = txts(2);
        assert!(
            CentroidCompressor::reduce(&v, &t, 0.2).is_empty(),
            "length mismatch must yield empty"
        );
    }

    /// Objective: Verify a wide epsilon merges, a tight epsilon splits.
    /// Invariants: same two vectors, eps=0.9 → 1 group; eps=1e-6 → 2 groups
    /// (exact duplicate excluded by distance==0 semantics? no — identical
    /// vectors always cluster; use near-but-distinct vectors).
    #[test]
    fn epsilon_controls_merging() {
        // Distinct-but-close directions.
        let vectors = vec![vec![1.0_f32, 0.0], vec![0.99_f32, 0.14]];
        let texts = txts(2);
        let loose = CentroidCompressor::reduce(&vectors, &texts, 0.1);
        assert_eq!(loose.len(), 1, "close vectors merge under loose epsilon");
        let tight = CentroidCompressor::reduce(&vectors, &texts, 0.001);
        assert_eq!(tight.len(), 2, "close vectors split under tight epsilon");
    }
}
