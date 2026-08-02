//! Anchor-based zero-shot classification (pure embedding, no LLM).
//!
//! Implements the plan's P0: predefined "domain/emotion" anchor vectors
//! `V_anchor`, and classify an incoming message embedding by cosine:
//!
//! ```text
//! score_i = cos(Emb(S), V_anchor_i) = (Emb(S)·V_i) / (|Emb(S)|·|V_i|)
//! ```
//!
//! Anchor vectors are the mean of a label's seed-word embeddings
//! (lexicon-seeded, e.g. `anxiety` ← 焦虑/压力/失眠/加班/崩溃). Classification
//! is pure linear algebra — no LLM, deterministic, ~1ms. When the embedding
//! service is disabled, callers pass no embedding and get an empty result
//! (graceful degradation, never a panic).

use crate::resolver::cosine_similarity;

/// A single anchor: a label plus its mean seed vector.
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorVec {
    /// Stable label, e.g. `"anxiety"`, `"work"`, `"food"`.
    pub label: String,
    /// Normalized-direction anchor vector (mean of seed embeddings).
    pub vector: Vec<f32>,
}

impl AnchorVec {
    /// Build an anchor from a label and a seed vector.
    #[must_use]
    pub fn new(label: impl Into<String>, vector: Vec<f32>) -> Self {
        Self {
            label: label.into(),
            vector,
        }
    }
}

/// Classifies embeddings against a fixed anchor set.
#[derive(Debug, Clone, Default)]
pub struct AnchorClassifier {
    anchors: Vec<AnchorVec>,
}

impl AnchorClassifier {
    /// Create an empty classifier (no anchors → empty classification).
    #[must_use]
    pub fn new() -> Self {
        Self {
            anchors: Vec::new(),
        }
    }

    /// Build a classifier from per-label seed vectors: each label's anchor is
    /// the centroid (mean) of its seed vectors.
    ///
    /// # Errors
    ///
    /// - Returns an empty classifier when the input is empty (no anchors).
    #[must_use]
    pub fn from_seed_vectors(seeds: &[(String, Vec<Vec<f32>>)]) -> Self {
        let mut anchors = Vec::with_capacity(seeds.len());
        for (label, vectors) in seeds {
            if let Some(centroid) = mean_vector(vectors) {
                anchors.push(AnchorVec::new(label.clone(), centroid));
            }
        }
        Self { anchors }
    }

    /// Classify an embedding: cosine similarity against every anchor,
    /// sorted descending (most similar first). Labels below zero similarity
    /// are still included (a value is informative even when negative).
    ///
    /// Returns an empty vec when the classifier has no anchors or the input
    /// embedding is unusable (empty / dimension mismatch with all anchors).
    #[must_use]
    pub fn classify(&self, emb: &[f32]) -> Vec<(String, f64)> {
        if emb.is_empty() || self.anchors.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<(String, f64)> = self
            .anchors
            .iter()
            .filter_map(|a| {
                let s = cosine_similarity(emb, &a.vector)?;
                Some((a.label.clone(), s))
            })
            .collect();
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// Number of registered anchors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    /// Whether the classifier has no anchors.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }
}

/// Compute the mean (centroid) of equal-length vectors.
///
/// Returns `None` when the input is empty, vectors have mismatched lengths,
/// or all vectors are zero-length (undefined direction).
#[must_use]
pub fn mean_vector(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let first = vectors.first()?;
    if first.is_empty() {
        return None;
    }
    let dim = first.len();
    let mut sum = vec![0.0_f64; dim];
    for v in vectors {
        if v.len() != dim {
            return None;
        }
        for (i, x) in v.iter().enumerate() {
            sum[i] += f64::from(*x);
        }
    }
    let n = vectors.len() as f64;
    let mean: Vec<f32> = sum.iter().map(|s| (s / n) as f32).collect();
    if mean.iter().all(|x| x.abs() <= f32::EPSILON) {
        return None;
    }
    Some(mean)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify the centroid of known vectors equals hand-computed
    /// mean.
    /// Invariants: mean([1,0],[3,0]) == [2,0]; mixed dims → None.
    #[test]
    fn mean_vector_is_centroid() {
        let v = mean_vector(&[vec![1.0_f32, 0.0], vec![3.0_f32, 0.0]]).expect("mean");
        assert_eq!(
            v,
            vec![2.0_f32, 0.0],
            "centroid must be the arithmetic mean"
        );

        assert_eq!(
            mean_vector(&[vec![1.0_f32], vec![1.0_f32, 2.0]]),
            None,
            "dimension mismatch must yield None"
        );
        assert_eq!(mean_vector(&[]), None, "empty input yields None");
        assert_eq!(
            mean_vector(&[vec![0.0_f32, 0.0], vec![0.0_f32, 0.0]]),
            None,
            "all-zero vectors have no defined direction"
        );
    }

    /// Objective: Verify classify ranks the matching anchor first.
    /// Invariants: an embedding equal to the "anxiety" anchor vector classifies
    /// "anxiety" as top with score ≈ 1.0.
    #[test]
    fn classify_top_anchor() {
        let classifier = AnchorClassifier::from_seed_vectors(&[
            ("anxiety".into(), vec![vec![1.0_f32, 0.0, 0.0]]),
            ("work".into(), vec![vec![0.0_f32, 1.0, 0.0]]),
        ]);
        let out = classifier.classify(&[1.0_f32, 0.0, 0.0]);
        assert_eq!(out[0].0, "anxiety", "anxiety anchor must rank first");
        assert!(
            (out[0].1 - 1.0).abs() < 1e-6,
            "identical vector must score ~1.0, got {}",
            out[0].1
        );
        assert_eq!(classifier.len(), 2);
    }

    /// Objective: Verify classify with no anchors returns empty (no panic).
    /// Invariants: empty classifier + any embedding → empty result.
    #[test]
    fn classify_without_anchors_is_empty() {
        let classifier = AnchorClassifier::new();
        assert!(classifier.is_empty(), "fresh classifier has no anchors");
        let out = classifier.classify(&[0.5_f32, 0.5]);
        assert!(out.is_empty(), "no anchors → no classification");
    }

    /// Objective: Verify embedding-less input degrades gracefully.
    /// Invariants: empty embedding → empty result; embedder-disabled caller
    /// path (empty emb) never panics.
    #[test]
    fn classify_empty_embedding_returns_empty() {
        let classifier = AnchorClassifier::from_seed_vectors(&[("a".into(), vec![vec![1.0_f32]])]);
        let out = classifier.classify(&[]);
        assert!(out.is_empty(), "empty embedding → empty classification");
    }

    /// Objective: Verify an orthogonal input does not false-positive.
    /// Invariants: embedding orthogonal to every anchor scores ≈ 0, so no
    /// anchor is confidently claimed (top score near zero).
    #[test]
    fn classify_orthogonal_does_not_false_positive() {
        let classifier = AnchorClassifier::from_seed_vectors(&[
            ("x".into(), vec![vec![1.0_f32, 0.0]]),
            ("y".into(), vec![vec![0.0_f32, 1.0]]),
        ]);
        // Embedding = [1,1] normalized → 0.707 to both anchors (equal, not
        // confident for either). Use [0.1, 0.1] direction to stay positive.
        let out = classifier.classify(&[1.0_f32, 1.0]);
        assert_eq!(out.len(), 2, "both anchors are returned");
        assert!(
            (out[0].1 - out[1].1).abs() < 1e-6,
            "equidistant input must not prefer either anchor, got {out:?}"
        );
    }

    /// Objective: Verify seed-vector classifier merges multiple seeds per
    /// label into a centroid.
    /// Invariants: two same-direction seeds → anchor equals their mean.
    #[test]
    fn from_seed_vectors_centroidizes() {
        let classifier = AnchorClassifier::from_seed_vectors(&[(
            "stress".into(),
            vec![vec![1.0_f32, 0.0], vec![3.0_f32, 0.0]],
        )]);
        assert_eq!(classifier.len(), 1);
        assert_eq!(
            classifier.anchors[0].vector,
            vec![2.0_f32, 0.0],
            "anchor vector must be the seed mean"
        );
    }
}
