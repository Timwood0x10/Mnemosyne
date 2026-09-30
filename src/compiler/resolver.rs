//! Vector computation layer — char-level embedding + similarity matching.
//!
//! Sits on top of the rule-based extraction pipeline. No LLM calls, no external
//! model dependencies — just character bigram Jaccard similarity.
//!
//! ## What it solves
//!
//! | Problem | Rule-only | + Vector layer |
//! |---------|-----------|---------------|
//! | "徐庶，字元直"  comma breakage | `take(4)` stops at comma → empty | bigram("徐庶") ≈ bigram("徐庶") → match ✅ |
//! | Noise list maintenance | 500-entry whack-a-mole | noise candidates have low similarity to known entity patterns |
//! | Alias variant matching | exact string only | "单福" ↔ "徐庶" via char overlap |

use std::collections::HashSet;

/// Minimum similarity threshold for entity name matching.
const MATCH_THRESHOLD: f64 = 0.45;

/// Build a set of character bigrams from a string.
fn bigrams(s: &str) -> HashSet<(char, char)> {
    let chars: Vec<char> = s.chars().collect();
    chars.windows(2).map(|w| (w[0], w[1])).collect()
}

/// Jaccard similarity between two sets of bigrams.
///
/// Two EMPTY bigram sets score `0.0`, not `1.0`: a 1-char name has no bigrams
/// (`chars().windows(2)` yields nothing), so every single-char name shares the
/// empty set and `1.0` would make any two of them a "full match" — the wrong
/// entity could then win `best_match`. Identical names keep scoring `1.0`
/// through the explicit identity shortcut in `best_match` / `score_between`,
/// which is the only place the original strings are still available.
fn jaccard(a: &HashSet<(char, char)>, b: &HashSet<(char, char)>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

/// Embed a name as a char-bigram vector (represented as its bigram set).
pub fn embed(name: &str) -> HashSet<(char, char)> {
    bigrams(name)
}

/// Compute cosine-style similarity between two name embeddings (Jaccard on bigrams).
///
/// Both-empty bigram sets return `0.0` (see [`jaccard`]); callers that hold the
/// raw strings should use [`NameIndex::score_between`] instead, which also
/// short-circuits byte-identical names to `1.0`.
pub fn similarity(a: &HashSet<(char, char)>, b: &HashSet<(char, char)>) -> f64 {
    jaccard(a, b)
}

/// A vector-space index of known entity names.
///
/// Built during initialization from the entity dictionary. Used during
/// Pass 2 to match auto-discovered names against known entities with
/// fuzzy similarity instead of exact string matching.
#[derive(Default)]
pub struct NameIndex {
    entries: Vec<(String, HashSet<(char, char)>)>,
}

impl NameIndex {
    /// Build index from a list of known names.
    pub fn build(names: &[String]) -> Self {
        NameIndex {
            entries: names.iter().map(|n| (n.clone(), bigrams(n))).collect(),
        }
    }

    /// Find the best matching canonical name for a candidate.
    /// Returns `None` if no match exceeds the threshold.
    ///
    /// A byte-identical name short-circuits to `1.0` BEFORE the Jaccard test:
    /// `jaccard` only sees bigram sets, so a 1-char name (empty bigram set)
    /// would otherwise score `0.0` even against itself.
    pub fn best_match(&self, candidate: &str) -> Option<(String, f64)> {
        let cand_vec = bigrams(candidate);
        let mut best: Option<(String, f64)> = None;
        for (name, vec) in &self.entries {
            if name == candidate {
                return Some((name.clone(), 1.0));
            }
            let sim = jaccard(&cand_vec, vec);
            if sim > MATCH_THRESHOLD {
                match &best {
                    Some((_, best_sim)) if sim > *best_sim => best = Some((name.clone(), sim)),
                    None => best = Some((name.clone(), sim)),
                    _ => {}
                }
            }
        }
        best
    }

    /// Get the similarity score between two names directly.
    ///
    /// Byte-identical names score `1.0` (identity shortcut); everything else is
    /// Jaccard-on-bigrams, where two empty bigram sets score `0.0`.
    pub fn score_between(&self, a: &str, b: &str) -> f64 {
        if a == b {
            return 1.0;
        }
        jaccard(&bigrams(a), &bigrams(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that "徐庶" and "徐庶，" have high similarity.
    /// Invariants: Jaccard ≥ 0.4 (they share all bigrams except comma noise).
    #[test]
    fn punctuation_noise_handled() {
        let idx = NameIndex::build(&["徐庶".into()]);
        let (name, sim) = idx.best_match("徐庶，").unwrap();
        assert_eq!(name, "徐庶");
        assert!(sim > 0.4, "similarity should be high despite comma");
    }

    /// Objective: Verify that dissimilar strings don't match.
    /// Invariants: best_match returns None below threshold.
    #[test]
    fn noise_rejected() {
        let idx = NameIndex::build(&["徐庶".into(), "刘备".into()]);
        assert!(
            idx.best_match("恐").is_none(),
            "noise word should not match"
        );
        assert!(
            idx.best_match("时").is_none(),
            "common word should not match"
        );
    }

    /// Objective: Verify the name index refuses to GUESS. "单福" is 徐庶's alias
    /// in the novel, but it shares no bigram with the canonical name, so the
    /// similarity gate must return `None` here — aliases are the entity
    /// registry's job, and the vector layer supplements exact matching instead of
    /// replacing it. An identical name still matches at full similarity.
    /// Invariants: zero shared bigrams → `None`; identical name → similarity 1.0.
    #[test]
    fn unlisted_alias_is_not_guessed() {
        let idx = NameIndex::build(&["徐庶".into(), "徐元直".into()]);
        assert_eq!(
            idx.best_match("单福"),
            None,
            "an alias sharing no bigram must not be force-matched"
        );
        assert_eq!(
            idx.best_match("徐元直"),
            Some(("徐元直".to_string(), 1.0)),
            "an identical name must match at full similarity"
        );
    }

    /// Objective: Verify that two DIFFERENT single-char names do not match.
    /// Invariants: both have empty bigram sets, so Jaccard is 0.0 and the
    /// threshold gate returns None — the wrong entity is never picked.
    #[test]
    fn distinct_single_char_names_do_not_match() {
        let idx = NameIndex::build(&["关".into()]);
        assert_eq!(
            idx.best_match("刘"),
            None,
            "different single-char names must not fully match"
        );
        assert_eq!(
            idx.score_between("刘", "关"),
            0.0,
            "two distinct empty-bigram names must score 0.0, not 1.0"
        );
    }

    /// Objective: Verify that an identical single-char name still fully matches.
    /// Invariants: the identity shortcut returns 1.0 before the Jaccard test,
    /// so a name is still found despite its empty bigram set.
    #[test]
    fn identical_single_char_name_matches() {
        let idx = NameIndex::build(&["关".into()]);
        assert_eq!(
            idx.best_match("关"),
            Some(("关".to_string(), 1.0)),
            "an identical single-char name must match at 1.0"
        );
        assert_eq!(
            idx.score_between("关", "关"),
            1.0,
            "identical names shortcut to 1.0"
        );
    }

    /// Objective: Verify that an empty candidate against an empty-bigram index
    /// does not produce a spurious 1.0 match (the both-empty Jaccard trap).
    /// Invariants: empty-vs-nonempty and empty-vs-empty score 0.0, so no
    /// best_match is returned; the raw Jaccard on two empty sets is 0.0.
    #[test]
    fn empty_candidate_is_not_fully_matched() {
        let idx = NameIndex::build(&["关".into(), "诸葛亮".into()]);
        assert_eq!(
            idx.best_match(""),
            None,
            "an empty candidate must not match any single-char name"
        );
        assert_eq!(
            similarity(&embed(""), &embed("")),
            0.0,
            "two empty bigram sets must score 0.0, not 1.0"
        );
    }
}
