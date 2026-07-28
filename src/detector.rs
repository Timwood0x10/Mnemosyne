//! Question and problem detection for the distillation pipeline.
//!
//! The detector decides whether a user message is a genuine problem statement
//! worth distilling, and whether an utterance is primarily a question. The
//! heuristics are intentionally lightweight and dependency-free so they can be
//! unit-tested deterministically.

use crate::types::Message;

/// Minimum content length for a message to be considered a candidate problem.
///
/// Very short messages ("ok", "hi", "yes") almost never encode extractable
/// problem-solution knowledge, so we filter them before scoring.
pub const MIN_PROBLEM_LENGTH: usize = 12;

/// Strong indicator phrases that mark a real problem statement.
///
/// Each entry is matched case-insensitively as a substring; the detector
/// short-circuits on the first hit so order does not matter for correctness
/// but does for micro-performance (most-common first is optimal).
const PROBLEM_INDICATORS: &[&str] = &[
    "how do i",
    "how to",
    "how can i",
    "what is the way to",
    "i need to",
    "i want to",
    "i'm trying to",
    "i am trying to",
    "why does",
    "why is",
    "why can't",
    "why cant",
    "error:",
    "exception:",
    "fails when",
    "panics when",
    "doesn't work",
    "does not work",
    "stuck on",
];

/// Disqualifier phrases that immediately reject a candidate problem.
///
/// These signal greeting, acknowledgment, or off-topic chat rather than a
/// knowledge-seeking question.
const NOISE_DISQUALIFIERS: &[&str] = &[
    "thank you",
    "thanks",
    "ok thanks",
    "got it",
    "sounds good",
    "that works",
    "perfect",
    "great",
    "awesome",
    "cool",
    "nice",
];

/// Detect whether a user message is a problem worth distilling.
///
/// Returns `true` when the message is non-trivial in length, is not pure
/// greeting/acknowledgment, and either contains a known problem indicator or
/// ends with a question mark (the cheap question heuristic).
///
/// # Arguments
///
/// * `msg` - A user message to evaluate. The caller is responsible for
///   ensuring the message has `role == "user"`; this function does not check.
///
/// # Examples
///
/// ```
/// use lore_scope::types::Message;
/// use lore_scope::detector::is_problem;
///
/// let q = Message::new("user", "How do I parse a JSON string in Rust?");
/// assert!(is_problem(&q));
///
/// let greeting = Message::new("user", "hi");
/// assert!(!is_problem(&greeting));
/// ```
#[must_use]
pub fn is_problem(msg: &Message) -> bool {
    let content = msg.content.trim();
    if content.len() < MIN_PROBLEM_LENGTH {
        return false;
    }
    let lower = content.to_lowercase();
    if NOISE_DISQUALIFIERS.iter().any(|d| lower.starts_with(d)) {
        return false;
    }
    if PROBLEM_INDICATORS.iter().any(|p| lower.contains(p)) {
        return true;
    }
    lower.ends_with('?') || lower.ends_with('？')
}

/// Lightweight question detector.
///
/// Wraps the cheap `ends_with('?')` heuristic together with a small set of
/// interrogative starters. Stateless and allocation-free in the hot path.
#[derive(Debug, Clone, Copy, Default)]
pub struct QuestionDetector;

impl QuestionDetector {
    /// Build a new detector (stateless, so all instances are equivalent).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Returns `true` when `text` looks like a question.
    ///
    /// The heuristic accepts:
    /// - text ending with `?`, or
    /// - text starting with one of `who|what|when|where|why|how|is|are|can|do|does`.
    #[must_use]
    pub fn is_question(&self, text: &str) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.ends_with('?') || trimmed.ends_with('？') {
            return true;
        }
        let lower = trimmed.to_lowercase();
        const INTERROGATIVES: &[&str] = &[
            "who ", "what ", "when ", "where ", "why ", "how ", "is ", "are ", "can ", "do ",
            "does ",
        ];
        INTERROGATIVES.iter().any(|p| lower.starts_with(p))
    }

    /// Returns `true` when the message body looks like a question.
    ///
    /// Convenience wrapper around [`QuestionDetector::is_question`].
    #[must_use]
    pub fn is_question_message(&self, msg: &Message) -> bool {
        self.is_question(&msg.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify is_problem accepts question-mark questions of adequate length.
    /// Invariants: Adequate-length question accepted; too-short question rejected.
    #[test]
    fn is_problem_accepts_question() {
        let q = Message::new("user", "How do I parse a JSON string in Rust?");
        assert!(is_problem(&q), "a long enough question should be a problem");
        let short = Message::new("user", "Why?");
        assert!(
            !is_problem(&short),
            "too-short question rejected by length gate"
        );
    }

    /// Objective: Verify is_problem rejects pure greetings and thank-yous.
    /// Invariants: Disqualifier prefixes always return false.
    #[test]
    fn is_problem_rejects_noise() {
        let thanks = Message::new("user", "thank you, this is very helpful to me");
        assert!(!is_problem(&thanks), "thank-you prefix should disqualify");
        let greeting = Message::new("user", "hello there, my good friend");
        assert!(!is_problem(&greeting), "short greeting below length gate");
    }

    /// Objective: Verify is_problem accepts problem-indicator substrings.
    /// Invariants: Each known indicator triggers a positive result.
    #[test]
    fn is_problem_accepts_indicators() {
        let cases = [
            ("error: cannot find function `foo`", true),
            ("How to read a file in Rust?", true),
            ("I'm trying to deploy a server but it fails", true),
            ("Cool, thanks again!", false),
        ];
        for (text, expected) in cases {
            let msg = Message::new("user", text);
            assert_eq!(
                is_problem(&msg),
                expected,
                "input `{text}` expected {expected}"
            );
        }
    }

    /// Objective: Verify QuestionDetector identifies obvious questions.
    /// Invariants: A trailing `?` always yields `true`.
    #[test]
    fn question_detector_trailing_mark() {
        let det = QuestionDetector::new();
        assert!(det.is_question("what is going on?"));
        assert!(det.is_question("really?"));
    }

    /// Objective: Verify QuestionDetector handles empty input safely.
    /// Invariants: Empty and whitespace-only strings return false.
    #[test]
    fn question_detector_empty_input() {
        let det = QuestionDetector::new();
        assert!(!det.is_question(""));
        assert!(!det.is_question("   "));
    }

    /// Objective: Verify QuestionDetector recognizes interrogative openers.
    /// Invariants: "how do I..." without a question mark still returns true.
    #[test]
    fn question_detector_interrogative_opener() {
        let det = QuestionDetector::new();
        assert!(det.is_question("how do I solve this"));
        assert!(det.is_question("why does it fail"));
        assert!(!det.is_question("the value is 42"));
    }

    /// Objective: Verify QuestionDetector wrapper over Message works identically.
    /// Invariants: is_question_message matches is_question on the same content.
    #[test]
    fn question_detector_message_wrapper() {
        let det = QuestionDetector::new();
        let msg = Message::new("user", "what is the capital of France?");
        assert!(det.is_question_message(&msg));
    }
}
