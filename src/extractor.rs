//! Experience extraction from conversation messages.
//!
//! [`ExperienceExtractor`] walks an ordered slice of [`Message`]s and produces
//! [`RawExperience`] candidates that the downstream pipeline (classify → score
//! → filter) can refine. Two extraction modes are supported:
//!
//! - **Direct**: pair each `user` problem message with the immediately
//!   following `assistant` message as its solution.
//! - **Cross-turn**: when a `user` message is followed by an `assistant`
//!   clarification request, look one turn further for the actual answer.
//!
//! The extractor is allocation-conservative: it builds its output `Vec` with
//! a capacity derived from the input length.

use crate::detector::{QuestionDetector, is_problem};
use crate::types::{ExtractionMethod, Message};

/// Working candidate produced by the extractor, before classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawExperience {
    /// The distilled problem statement.
    pub problem: String,
    /// The distilled solution / answer.
    pub solution: String,
    /// How this experience was extracted.
    pub method: ExtractionMethod,
}

/// Configuration knobs for the extractor.
#[derive(Debug, Clone, Copy)]
pub struct ExtractorConfig {
    /// Enable cross-turn extraction.
    pub enable_cross_turn: bool,
}

impl Default for ExtractorConfig {
    fn default() -> Self {
        Self {
            enable_cross_turn: true,
        }
    }
}

/// Stateless experience extractor.
#[derive(Debug, Clone, Copy)]
pub struct ExperienceExtractor {
    cfg: ExtractorConfig,
    question_detector: QuestionDetector,
}

impl ExperienceExtractor {
    /// Build a new extractor with the given configuration.
    #[must_use]
    pub fn new(cfg: ExtractorConfig) -> Self {
        Self {
            cfg,
            question_detector: QuestionDetector::new(),
        }
    }

    /// Build an extractor with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(ExtractorConfig::default())
    }

    /// Extract raw experiences from a slice of messages.
    ///
    /// Returns an empty vector for empty input. Messages must be in
    /// chronological order; the extractor relies on adjacency to pair
    /// problems with solutions.
    ///
    /// # Arguments
    ///
    /// * `messages` - Ordered conversation messages.
    #[must_use]
    pub fn extract(&self, messages: &[Message]) -> Vec<RawExperience> {
        if messages.is_empty() {
            return Vec::new();
        }
        // Upper bound on experiences: at most one per user message.
        let user_count = messages.iter().filter(|m| m.is_user()).count();
        let mut out = Vec::with_capacity(user_count);

        let mut i = 0;
        while i < messages.len() {
            let msg = &messages[i];
            if !msg.is_user() {
                i += 1;
                continue;
            }
            if !is_problem(msg) {
                i += 1;
                continue;
            }

            // Look ahead for the assistant answer.
            if let Some(exp) = self.try_direct_extract(messages, i) {
                out.push(exp);
                i += 2;
                continue;
            }

            // Cross-turn: assistant asks for clarification, real answer follows.
            if self.cfg.enable_cross_turn
                && let Some(exp) = self.try_cross_turn_extract(messages, i) {
                    out.push(exp);
                    // Skip past the second assistant message.
                    i = self
                        .next_after(messages, i + 1, "assistant")
                        .unwrap_or(messages.len())
                        + 1;
                    continue;
                }

            i += 1;
        }

        out
    }

    /// Attempt a direct `user -> assistant` extraction starting at `user_idx`.
    fn try_direct_extract(&self, messages: &[Message], user_idx: usize) -> Option<RawExperience> {
        let asst_idx = user_idx + 1;
        if asst_idx >= messages.len() {
            return None;
        }
        let asst = &messages[asst_idx];
        if !asst.is_assistant() {
            return None;
        }
        // If the assistant message is itself a clarification question,
        // direct extraction yields low-quality pairs — defer to cross-turn.
        if self.question_detector.is_question(&asst.content) {
            return None;
        }
        Some(RawExperience {
            problem: messages[user_idx].content.clone(),
            solution: asst.content.clone(),
            method: ExtractionMethod::Direct,
        })
    }

    /// Attempt a cross-turn extraction.
    ///
    /// Pattern: `user -> assistant(clarification) -> user(answer) -> assistant(answer)`.
    /// We treat the original `user` message as the problem and the final
    /// `assistant` message as the solution.
    fn try_cross_turn_extract(
        &self,
        messages: &[Message],
        user_idx: usize,
    ) -> Option<RawExperience> {
        let asst_clarify_idx = user_idx + 1;
        if asst_clarify_idx >= messages.len() {
            return None;
        }
        let asst_clarify = &messages[asst_clarify_idx];
        if !asst_clarify.is_assistant() {
            return None;
        }
        if !self.question_detector.is_question(&asst_clarify.content) {
            // Assistant didn't ask a clarification — not a cross-turn case.
            return None;
        }
        // Find the next assistant message after the clarification; the user
        // message between them is the user's answer to the clarification,
        // which we don't store separately (it's folded into the solution
        // context if needed by downstream stages).
        let next_asst_idx = self.next_after(messages, asst_clarify_idx, "assistant")?;
        let final_asst = &messages[next_asst_idx];
        if self.question_detector.is_question(&final_asst.content) {
            // Another clarification — bail out; chain too long.
            return None;
        }
        Some(RawExperience {
            problem: messages[user_idx].content.clone(),
            solution: final_asst.content.clone(),
            method: ExtractionMethod::CrossTurn,
        })
    }

    /// Returns the index of the next message with the given role after
    /// `start_idx`, or `None` if there is no such message.
    fn next_after(&self, messages: &[Message], start_idx: usize, role: &str) -> Option<usize> {
        messages
            .iter()
            .enumerate()
            .skip(start_idx + 1)
            .find(|(_, m)| m.role == role)
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify empty input produces an empty result without panic.
    /// Invariants: extract(&[]) returns an empty Vec.
    #[test]
    fn extract_empty() {
        let e = ExperienceExtractor::with_defaults();
        assert!(e.extract(&[]).is_empty(), "empty input -> empty output");
    }

    /// Objective: Verify a single direct user→assistant pair is extracted.
    /// Invariants: One RawExperience with method == Direct.
    #[test]
    fn extract_direct_pair() {
        let e = ExperienceExtractor::with_defaults();
        let msgs = vec![
            Message::new("user", "How do I parse JSON in Rust?"),
            Message::new("assistant", "Use serde_json::from_str."),
        ];
        let result = e.extract(&msgs);
        assert_eq!(result.len(), 1, "one experience extracted");
        assert_eq!(
            result[0].method,
            ExtractionMethod::Direct,
            "method is Direct"
        );
        assert_eq!(result[0].problem, "How do I parse JSON in Rust?");
        assert_eq!(result[0].solution, "Use serde_json::from_str.");
    }

    /// Objective: Verify non-problem user messages are skipped.
    /// Invariants: A short "ok" user message yields no experience.
    #[test]
    fn extract_skips_non_problem() {
        let e = ExperienceExtractor::with_defaults();
        let msgs = vec![
            Message::new("user", "ok"), // too short / chatter
            Message::new("assistant", "Great."),
        ];
        assert!(
            e.extract(&msgs).is_empty(),
            "non-problem user messages skipped"
        );
    }

    /// Objective: Verify cross-turn extraction produces CrossTurn method.
    /// Invariants: Clarification pattern yields one CrossTurn experience.
    #[test]
    fn extract_cross_turn() {
        let e = ExperienceExtractor::with_defaults();
        let msgs = vec![
            Message::new("user", "How do I fix the network error in my Rust server?"),
            Message::new("assistant", "Could you share the exact error message?"),
            Message::new("user", "It says connection refused on port 8080."),
            Message::new(
                "assistant",
                "That means nothing is listening on the port; start your server first.",
            ),
        ];
        let result = e.extract(&msgs);
        assert_eq!(result.len(), 1, "one cross-turn experience");
        assert_eq!(
            result[0].method,
            ExtractionMethod::CrossTurn,
            "method is CrossTurn"
        );
        assert_eq!(
            result[0].solution,
            "That means nothing is listening on the port; start your server first."
        );
    }

    /// Objective: Verify cross-turn extraction is disabled by config.
    /// Invariants: With enable_cross_turn=false, clarification pattern yields no experience.
    #[test]
    fn extract_cross_turn_disabled() {
        let cfg = ExtractorConfig {
            enable_cross_turn: false,
        };
        let e = ExperienceExtractor::new(cfg);
        let msgs = vec![
            Message::new("user", "How do I fix the network error?"),
            Message::new("assistant", "Could you share the exact error?"),
        ];
        // Direct extraction will bail because assistant message is a question.
        assert!(
            e.extract(&msgs).is_empty(),
            "cross-turn disabled -> no experience"
        );
    }

    /// Objective: Verify multiple direct pairs in sequence.
    /// Invariants: Two user→assistant turns yield two experiences.
    #[test]
    fn extract_multiple_pairs() {
        let e = ExperienceExtractor::with_defaults();
        let msgs = vec![
            Message::new("user", "How do I read a file?"),
            Message::new("assistant", "Use std::fs::read_to_string."),
            Message::new("user", "How do I write a file?"),
            Message::new("assistant", "Use std::fs::write."),
        ];
        let result = e.extract(&msgs);
        assert_eq!(result.len(), 2, "two direct experiences extracted");
        assert!(
            result.iter().all(|r| r.method == ExtractionMethod::Direct),
            "all direct"
        );
    }

    /// Objective: Verify direct extraction skips when assistant answer is itself a question.
    /// Invariants: Direct extract returns None if assistant message is a question.
    #[test]
    fn direct_extract_bails_on_clarification_question() {
        let e = ExperienceExtractor::with_defaults();
        let msgs = vec![
            Message::new("user", "How do I fix this error?"),
            Message::new("assistant", "What error are you seeing?"),
        ];
        // With cross-turn enabled but no further assistant, extract yields nothing.
        assert!(
            e.extract(&msgs).is_empty(),
            "single clarification pair yields nothing"
        );
    }
}
