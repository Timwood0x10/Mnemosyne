//! Conflict detection and resolution for distilled memories.
//!
//! When a new memory is sufficiently similar (cosine similarity ≥
//! [`ConflictResolver::threshold`]) to an existing memory of the same type,
//! the resolver decides whether to replace the old memory or keep both.
//!
//! Replacement strategy:
//! - If the new memory's `importance` is strictly greater than the existing
//!   one's `confidence`, replace.
//! - Otherwise keep both.
//!
//! Embedding fallback: when either memory carries no embedding vector (the
//! keyword-only configuration, or a failed embedding call), cosine comparison
//! is impossible. The resolver then falls back to a deterministic
//! content-similarity score (character-bigram Jaccard) so duplicate or
//! near-duplicate memories are still caught instead of silently piling up.
//!
//! The resolver is stateless apart from its threshold; the existing-memory
//! lookup is delegated to the [`crate::store::ExperienceRepository`] trait
//! by the distiller, while this module only computes the decision.

use std::collections::HashSet;

use crate::types::Memory;

/// Outcome of a conflict resolution decision.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// No conflict; the new memory should be inserted as-is.
    NoConflict,
    /// The new memory replaces an existing one whose id is stored here.
    ReplaceOld {
        /// Identifier of the memory to remove from the store.
        old_id: String,
        /// The new memory that supersedes the old one.
        new_memory: Box<Memory>,
    },
    /// The new memory is sufficiently different; keep both.
    KeepBoth,
}

/// Stateless conflict resolver with a configurable similarity threshold.
#[derive(Debug, Clone, Copy)]
pub struct ConflictResolver {
    /// Cosine similarity above which two memories are considered conflicting.
    threshold: f64,
}

impl ConflictResolver {
    /// Build a new resolver with the given cosine-similarity threshold.
    ///
    /// # Panics
    ///
    /// Panics if `threshold` is outside `[0.0, 1.0]`. This is a programmer
    /// error caught at construction time, not a runtime condition.
    #[must_use]
    pub fn new(threshold: f64) -> Self {
        assert!(
            (0.0..=1.0).contains(&threshold),
            "conflict threshold must be in [0, 1], got {threshold}"
        );
        Self { threshold }
    }

    /// Returns the configured similarity threshold.
    #[must_use]
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Decide whether `new_memory` conflicts with `existing`.
    ///
    /// Conflict requires:
    /// 1. A similarity score ≥ `threshold`. With embeddings present that is
    ///    the cosine similarity of the two vectors; when either vector is
    ///    missing (keyword-only mode, or a failed embedding call) the score
    ///    falls back to [`content_similarity`] over the memory texts so
    ///    duplicates are still caught.
    /// 2. Both memories share the same [`MemoryType`].
    ///
    /// When a conflict is detected the decision is `ReplaceOld` if the new
    /// memory's importance is strictly greater than the existing one's
    /// `confidence`; otherwise the decision is `KeepBoth`.
    ///
    /// # Arguments
    ///
    /// * `new_memory` - The freshly distilled memory candidate.
    /// * `existing` - An existing memory already in the store, with its
    ///   embedding vector populated.
    /// * `existing_confidence` - The stored confidence/importance of the
    ///   existing memory.
    #[must_use]
    pub fn resolve(
        &self,
        new_memory: &Memory,
        existing: &Memory,
        existing_confidence: f64,
    ) -> Resolution {
        let similarity = match cosine_similarity(&new_memory.vector, &existing.vector) {
            Some(s) => s,
            // Embedding fallback: no comparable vectors (empty, mismatched, or
            // zero-magnitude) → deterministic content similarity so duplicate
            // memories are caught even in keyword-only mode instead of every
            // distilled round inserting another near-identical row.
            None => content_similarity(&new_memory.content, &existing.content),
        };
        if similarity < self.threshold {
            return Resolution::NoConflict;
        }
        if new_memory.memory_type != existing.memory_type {
            // Different type: not a conflict for replacement purposes.
            return Resolution::KeepBoth;
        }
        if new_memory.importance > existing_confidence {
            Resolution::ReplaceOld {
                old_id: existing.id.clone(),
                new_memory: Box::new(new_memory.clone()),
            }
        } else {
            Resolution::KeepBoth
        }
    }

    /// Convenience wrapper: returns `true` when the cosine similarity between
    /// the two vectors is ≥ `threshold` and vectors are compatible.
    ///
    /// Use this when you only need the boolean conflict signal and don't
    /// care about the replacement decision.
    #[must_use]
    pub fn is_conflict(&self, a: &[f32], b: &[f32]) -> bool {
        cosine_similarity(a, b)
            .map(|s| s >= self.threshold)
            .unwrap_or(false)
    }
}

/// Compute cosine similarity between two vectors.
///
/// Returns `None` when:
/// - either slice is empty, or
/// - the slices differ in length, or
/// - both vectors have zero magnitude (division by zero guard).
///
/// Otherwise returns a value in `[-1.0, 1.0]`.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    if a.len() != b.len() {
        return None;
    }
    let mut dot: f64 = 0.0;
    let mut norm_a: f64 = 0.0;
    let mut norm_b: f64 = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        let xf = f64::from(*x);
        let yf = f64::from(*y);
        dot += xf * yf;
        norm_a += xf * xf;
        norm_b += yf * yf;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return None;
    }
    Some(dot / (norm_a.sqrt() * norm_b.sqrt()))
}

/// Compute a deterministic content-similarity score in `[0.0, 1.0]` between
/// two texts, defined as the **containment** of the shorter text's character
/// bigrams in the longer one: `|A∩B| / min(|A|,|B|)`.
///
/// This is the embedding-free fallback for [`ConflictResolver::resolve`]:
/// identical texts score 1.0, texts sharing no bigram score 0.0, and any
/// text too short to form a bigram scores 0.0 against everything except an
/// exact match. Containment — rather than Dice/Jaccard — is deliberate:
/// a duplicate memory is typically the same record with an appended clause
/// ("…修复。clippy 0 warning。"), which collapses symmetric coefficients
/// below the conflict threshold even though one text fully contains the
/// other. A length-ratio gate (`< 0.5`) rejects the opposite failure mode,
/// where a short fragment ("曹操") would otherwise "contain" a long sentence
/// ("曹操知人善任") and be over-merged into a different memory.
#[must_use]
pub fn content_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let bigrams = |s: &str| -> HashSet<(char, char)> {
        let chars: Vec<char> = s.chars().collect();
        chars.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let ga = bigrams(a);
    let gb = bigrams(b);
    if ga.is_empty() || gb.is_empty() {
        return 0.0;
    }
    let shorter = ga.len().min(gb.len());
    let longer = ga.len().max(gb.len());
    // Length-ratio gate: a fragment embedded in a much longer text is a
    // different memory, not a near-duplicate of it.
    if shorter as f64 / (longer as f64) < 0.5 {
        return 0.0;
    }
    let intersection = ga.intersection(&gb).count();
    let containment = intersection as f64 / shorter as f64;
    containment.min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryType;

    fn mem_with_vector(tenant: &str, mt: MemoryType, vec: Vec<f32>, importance: f64) -> Memory {
        let mut m = Memory::new(tenant, mt, "x", importance);
        m.vector = vec;
        m
    }

    /// Objective: Verify identical vectors yield cosine similarity 1.0.
    /// Invariants: cosine_similarity(v, v) == 1.0 for nonzero v.
    #[test]
    fn cosine_identical_vectors() {
        let v = vec![1.0_f32, 2.0, 3.0];
        let s = cosine_similarity(&v, &v).expect("nonzero vector");
        assert!(
            (s - 1.0).abs() < 1e-9,
            "identical nonzero vectors -> cosine = 1.0, got {s}"
        );
    }

    /// Objective: Verify orthogonal vectors yield cosine similarity 0.0.
    /// Invariants: cosine_similarity([1,0],[0,1]) == 0.0.
    #[test]
    fn cosine_orthogonal_vectors() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        let s = cosine_similarity(&a, &b).expect("nonzero vectors");
        assert!(
            s.abs() < 1e-9,
            "orthogonal vectors -> cosine = 0.0, got {s}"
        );
    }

    /// Objective: Verify dimension mismatch returns None.
    /// Invariants: cosine_similarity refuses mismatched vector lengths.
    #[test]
    fn cosine_dimension_mismatch() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![1.0_f32, 2.0];
        assert!(
            cosine_similarity(&a, &b).is_none(),
            "mismatched lengths -> None"
        );
    }

    /// Objective: Verify empty vectors return None.
    /// Invariants: cosine_similarity refuses empty vectors.
    #[test]
    fn cosine_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert!(cosine_similarity(&a, &b).is_none(), "empty vectors -> None");
    }

    /// Objective: Verify zero-magnitude vectors return None.
    /// Invariants: cosine_similarity refuses zero-magnitude inputs (div by zero guard).
    #[test]
    fn cosine_zero_magnitude() {
        let a = vec![0.0_f32, 0.0];
        let b = vec![1.0_f32, 2.0];
        assert!(
            cosine_similarity(&a, &b).is_none(),
            "zero-magnitude -> None"
        );
    }

    /// Objective: Verify high-similarity vectors trigger ReplaceOld when new importance is higher.
    /// Invariants: Resolve yields ReplaceOld with the correct old_id.
    #[test]
    fn resolve_replaces_when_new_importance_higher() {
        let resolver = ConflictResolver::new(0.85);
        let v = vec![1.0_f32, 0.0, 0.0];
        let existing = mem_with_vector("t1", MemoryType::Knowledge, v.clone(), 0.5);
        let new_memory = mem_with_vector("t1", MemoryType::Knowledge, v.clone(), 0.9);
        let old_id = existing.id.clone();
        let resolution = resolver.resolve(&new_memory, &existing, 0.5);
        match resolution {
            Resolution::ReplaceOld {
                old_id: oid,
                new_memory: _,
            } => {
                assert_eq!(oid, old_id, "old_id matches existing memory");
            }
            other => panic!("expected ReplaceOld, got {other:?}"),
        }
    }

    /// Objective: Verify high-similarity but lower-importance new memory yields KeepBoth.
    /// Invariants: Resolve yields KeepBoth when new importance <= existing confidence.
    #[test]
    fn resolve_keep_both_when_new_importance_lower() {
        let resolver = ConflictResolver::new(0.85);
        let v = vec![1.0_f32, 0.0, 0.0];
        let existing = mem_with_vector("t1", MemoryType::Knowledge, v.clone(), 0.5);
        let new_memory = mem_with_vector("t1", MemoryType::Knowledge, v.clone(), 0.3);
        let resolution = resolver.resolve(&new_memory, &existing, 0.5);
        assert_eq!(
            resolution,
            Resolution::KeepBoth,
            "lower importance -> KeepBoth"
        );
    }

    /// Objective: Verify low-similarity vectors yield NoConflict.
    /// Invariants: When cosine < threshold, decision is NoConflict.
    #[test]
    fn resolve_no_conflict_on_low_similarity() {
        let resolver = ConflictResolver::new(0.85);
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0]; // orthogonal -> cosine = 0
        let existing = mem_with_vector("t1", MemoryType::Knowledge, b, 0.5);
        let new_memory = mem_with_vector("t1", MemoryType::Knowledge, a, 0.9);
        let resolution = resolver.resolve(&new_memory, &existing, 0.5);
        assert_eq!(
            resolution,
            Resolution::NoConflict,
            "low similarity -> NoConflict"
        );
    }

    /// Objective: Verify mismatched memory types yield KeepBoth even at high similarity.
    /// Invariants: Knowledge vs Preference at cosine=1.0 -> KeepBoth.
    #[test]
    fn resolve_keep_both_on_type_mismatch() {
        let resolver = ConflictResolver::new(0.85);
        let v = vec![1.0_f32, 0.0, 0.0];
        let existing = mem_with_vector("t1", MemoryType::Knowledge, v.clone(), 0.5);
        let new_memory = mem_with_vector("t1", MemoryType::Preference, v.clone(), 0.9);
        let resolution = resolver.resolve(&new_memory, &existing, 0.5);
        assert_eq!(
            resolution,
            Resolution::KeepBoth,
            "type mismatch -> KeepBoth"
        );
    }

    /// Objective: Verify empty vectors yield NoConflict (not a panic).
    /// Invariants: resolve on empty-vector memories returns NoConflict.
    #[test]
    fn resolve_no_conflict_on_empty_vectors() {
        let resolver = ConflictResolver::new(0.85);
        let existing = Memory::new("t1", MemoryType::Knowledge, "x", 0.5);
        let new_memory = Memory::new("t1", MemoryType::Knowledge, "y", 0.9);
        let resolution = resolver.resolve(&new_memory, &existing, 0.5);
        assert_eq!(
            resolution,
            Resolution::NoConflict,
            "empty vectors -> NoConflict"
        );
    }

    /// Objective: Verify identical texts score 1.0 (exact-match shortcut).
    /// Invariants: content_similarity(a, a) == 1.0 for any non-empty text.
    #[test]
    fn content_similarity_identical_texts() {
        let s = content_similarity("评估发现三个阻塞项：SSE串台", "评估发现三个阻塞项：SSE串台");
        assert!(
            (s - 1.0).abs() < f64::EPSILON,
            "identical texts -> 1.0, got {s}"
        );
    }

    /// Objective: Verify disjoint texts score 0.0.
    /// Invariants: no shared bigram -> 0.0.
    #[test]
    fn content_similarity_disjoint_texts() {
        let s = content_similarity("曹操知人善任", "孙权优柔寡断");
        assert!(s < 0.05, "disjoint texts -> near 0.0, got {s}");
    }

    /// Objective: Verify short texts (no bigram) score 0.0 against longer
    /// texts, and identical short texts still score 1.0 via the exact match.
    /// Invariants: single char vs long text -> 0.0; "x" vs "x" -> 1.0.
    #[test]
    fn content_similarity_short_texts() {
        assert_eq!(
            content_similarity("x", "评估发现三个阻塞项"),
            0.0,
            "no shared bigram with a single-char text"
        );
        assert_eq!(
            content_similarity("x", "x"),
            1.0,
            "identical single-char texts still match exactly"
        );
    }

    /// Objective: Verify near-duplicate memories are caught without vectors.
    /// Invariants: empty vectors + identical content + higher importance
    /// -> ReplaceOld (previously NoConflict — the duplicate silently piled up).
    #[test]
    fn resolve_replaces_duplicate_content_without_vectors() {
        let resolver = ConflictResolver::new(0.85);
        let content =
            "评估发现三个阻塞项：多客户端SSE串台、HTTP默认无鉴权、path白名单。修复：SSE会话隔离。";
        let existing = Memory::new("t1", MemoryType::Knowledge, content, 0.5);
        let new_memory = Memory::new("t1", MemoryType::Knowledge, content, 0.9);
        let old_id = existing.id.clone();
        let resolution = resolver.resolve(&new_memory, &existing, 0.5);
        match resolution {
            Resolution::ReplaceOld {
                old_id: oid,
                new_memory: _,
            } => {
                assert_eq!(oid, old_id, "old_id matches existing memory");
            }
            other => panic!("expected ReplaceOld on duplicate content, got {other:?}"),
        }
    }

    /// Objective: Verify near-duplicate content (not byte-identical) also
    /// trips the conflict path via bigram Jaccard similarity.
    /// Invariants: one-word edit keeps similarity above the threshold.
    #[test]
    fn resolve_conflicts_on_near_duplicate_content_without_vectors() {
        let resolver = ConflictResolver::new(0.85);
        let existing = Memory::new(
            "t1",
            MemoryType::Knowledge,
            "评估发现三个阻塞项：多客户端SSE串台、HTTP默认无鉴权、path白名单。修复：SSE会话隔离。",
            0.5,
        );
        let new_memory = Memory::new(
            "t1",
            MemoryType::Knowledge,
            "评估发现三个阻塞项：多客户端SSE串台、HTTP默认无鉴权、path白名单。修复：SSE会话隔离。clippy 0 warning。",
            0.9,
        );
        let resolution = resolver.resolve(&new_memory, &existing, 0.5);
        assert!(
            matches!(resolution, Resolution::ReplaceOld { .. }),
            "near-duplicate content must conflict, got {resolution:?}"
        );
    }

    /// Objective: Verify clearly different content still yields NoConflict
    /// even with empty vectors (the fallback must not over-merge).
    /// Invariants: disjoint texts -> NoConflict.
    #[test]
    fn resolve_no_conflict_on_distinct_content_without_vectors() {
        let resolver = ConflictResolver::new(0.85);
        let existing = Memory::new("t1", MemoryType::Knowledge, "孙权为什么你不喜欢？", 0.5);
        let new_memory = Memory::new("t1", MemoryType::Knowledge, "Linux编译太慢了怎么办？", 0.9);
        let resolution = resolver.resolve(&new_memory, &existing, 0.5);
        assert_eq!(
            resolution,
            Resolution::NoConflict,
            "disjoint contents -> NoConflict"
        );
    }

    /// Objective: Verify threshold getter returns the configured value.
    /// Invariants: threshold() matches the value passed to new().
    #[test]
    fn threshold_getter() {
        let r = ConflictResolver::new(0.92);
        assert!(
            (r.threshold() - 0.92).abs() < f64::EPSILON,
            "threshold getter should return configured value"
        );
    }

    /// Objective: Verify is_conflict wrapper agrees with full resolve.
    /// Invariants: is_conflict returns true iff cosine >= threshold.
    #[test]
    fn is_conflict_wrapper() {
        let resolver = ConflictResolver::new(0.85);
        let a = vec![1.0_f32, 0.0];
        let b = vec![1.0_f32, 0.0]; // identical -> cosine = 1.0
        let c = vec![0.0_f32, 1.0]; // orthogonal -> cosine = 0.0
        assert!(resolver.is_conflict(&a, &b), "identical -> conflict");
        assert!(!resolver.is_conflict(&a, &c), "orthogonal -> no conflict");
    }
}
