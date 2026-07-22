//! Memory classification heuristics.
//!
//! [`MemoryClassifier`] assigns one of [`MemoryType`] to a `(problem, solution)`
//! pair using lightweight keyword scoring. The scoring is deterministic and
//! allocation-free in the hot path, which makes the classifier trivially
//! unit-testable.

use crate::types::MemoryType;

/// Keyword weights per memory type.
///
/// The array is a small, hand-curated list of representative keywords for
/// each type. The classifier sums matches across all types and picks the
/// winner; on ties it falls back to the implicit array order.
const TYPE_KEYWORDS: [(MemoryType, &[&str]); 4] = [
    (
        MemoryType::Profile,
        &[
            "i am",
            "i'm",
            "my name",
            "i work",
            "i use",
            "i prefer",
            "i like",
            "i love",
            "i hate",
            "i live",
            "i'm based",
            "i'm from",
            "my role",
            "my job",
        ],
    ),
    (
        MemoryType::Preference,
        &[
            "prefer",
            "always use",
            "never use",
            "should use",
            "favorite",
            "favourite",
            "convention",
            "style guide",
            "i like to",
            "i dislike",
            "tab indentation",
            "snake_case",
            "camelcase",
        ],
    ),
    (
        MemoryType::Interaction,
        &[
            "today",
            "yesterday",
            "this morning",
            "right now",
            "let's",
            "let us",
            "in this chat",
            "earlier",
            "last time",
            "later today",
            "this week",
            "this session",
        ],
    ),
    (
        MemoryType::Knowledge,
        &[
            "how to",
            "how do i",
            "error",
            "exception",
            "fix",
            "solution",
            "cause",
            "because",
            "the problem is",
            "root cause",
            "workaround",
            "deprecated",
        ],
    ),
];

/// Stateless memory classifier.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryClassifier;

impl MemoryClassifier {
    /// Build a new classifier instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Classify a `(problem, solution)` pair into a [`MemoryType`].
    ///
    /// The algorithm:
    /// 1. Concatenate `problem` and `solution` and lowercase.
    /// 2. For each `(type, keywords)` pair, count substring matches.
    /// 3. Pick the type with the highest score; tie-break by array order
    ///    (Profile > Preference > Interaction > Knowledge).
    /// 4. If no keyword matched anywhere, default to `Knowledge`.
    ///
    /// # Arguments
    ///
    /// * `problem` - The distilled problem statement.
    /// * `solution` - The distilled solution / answer.
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_distill::classifier::MemoryClassifier;
    /// use memory_distill::types::MemoryType;
    ///
    /// let c = MemoryClassifier::new();
    /// let t = c.classify("How do I parse JSON in Rust?", "Use serde_json::from_str.");
    /// assert_eq!(t, MemoryType::Knowledge);
    /// ```
    #[must_use]
    pub fn classify(&self, problem: &str, solution: &str) -> MemoryType {
        let combined_lower = format!("{} {}", problem.to_lowercase(), solution.to_lowercase());

        let mut best_type = MemoryType::Knowledge;
        let mut best_score: usize = 0;

        for (mem_type, keywords) in TYPE_KEYWORDS {
            let score = keywords
                .iter()
                .filter(|kw| combined_lower.contains(*kw))
                .count();
            if score > best_score {
                best_score = score;
                best_type = mem_type;
            }
        }

        best_type
    }

    /// Classify a single text blob.
    ///
    /// Convenience wrapper used by the extractor when the problem/solution
    /// split has not yet been made.
    #[must_use]
    pub fn classify_text(&self, text: &str) -> MemoryType {
        self.classify(text, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that how-to phrasing classifies as Knowledge.
    /// Invariants: Knowledge keywords dominate on "how to ... fix ... error".
    #[test]
    fn classify_how_to_is_knowledge() {
        let c = MemoryClassifier::new();
        let t = c.classify(
            "How to fix the borrow checker error in Rust?",
            "Use cloning or restructure lifetimes.",
        );
        assert_eq!(t, MemoryType::Knowledge, "how-to/fix/error -> Knowledge");
    }

    /// Objective: Verify that explicit user-identity phrasing classifies as Profile.
    /// Invariants: "I am ... I use ..." should win over weaker signals.
    #[test]
    fn classify_identity_is_profile() {
        let c = MemoryClassifier::new();
        let t = c.classify("I am a Rust developer.", "I use Rust daily.");
        assert_eq!(t, MemoryType::Profile, "identity statements -> Profile");
    }

    /// Objective: Verify preference keywords win when present.
    /// Invariants: "prefer snake_case" yields Preference.
    #[test]
    fn classify_preference() {
        let c = MemoryClassifier::new();
        let t = c.classify(
            "I prefer snake_case for variables.",
            "This is my favorite style.",
        );
        assert_eq!(
            t,
            MemoryType::Preference,
            "prefer/favorite/snake_case -> Preference"
        );
    }

    /// Objective: Verify interaction keywords win on time-bound utterances.
    /// Invariants: "today ... this week" yields Interaction.
    #[test]
    fn classify_interaction() {
        let c = MemoryClassifier::new();
        let t = c.classify(
            "What did we do today in this session?",
            "We worked on the chat earlier this week.",
        );
        assert_eq!(t, MemoryType::Interaction, "today/this week -> Interaction");
    }

    /// Objective: Verify fallback to Knowledge on signal-free input.
    /// Invariants: When no keywords match, default is Knowledge.
    #[test]
    fn classify_no_signal_falls_back_to_knowledge() {
        let c = MemoryClassifier::new();
        let t = c.classify("The sky is blue.", "Water flows downstream.");
        assert_eq!(t, MemoryType::Knowledge, "no signal -> Knowledge default");
    }

    /// Objective: Verify classify_text matches classify with empty solution.
    /// Invariants: Both APIs agree on identical text input.
    #[test]
    fn classify_text_matches_classify() {
        let c = MemoryClassifier::new();
        let text = "I like to use Rust for systems programming.";
        assert_eq!(
            c.classify_text(text),
            c.classify(text, ""),
            "classify_text must equal classify with empty solution"
        );
    }

    /// Objective: Verify case-insensitive matching (input uppercase).
    /// Invariants: Uppercase keyword matches as well as lowercase.
    #[test]
    fn classify_case_insensitive() {
        let c = MemoryClassifier::new();
        let t = c.classify("HOW TO FIX THIS ERROR?", "USE A WORKAROUND.");
        assert_eq!(
            t,
            MemoryType::Knowledge,
            "uppercase input should still match"
        );
    }
}
