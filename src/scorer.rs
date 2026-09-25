//! Importance scoring for distilled memories.
//!
//! [`ImportanceScorer`] assigns a score in `[0.0, 1.0]` to a `(problem, solution)`
//! pair by combining three signals:
//!
//! 1. **Keyword density**: presence of high-value keywords.
//! 2. **Memory type**: Knowledge and Profile score higher than Interaction.
//! 3. **Length**: a sweet-spot length scores higher than very short or very long.
//!
//! All weights are exposed as named constants so they can be tuned without
//! rewriting the algorithm.

use crate::types::MemoryType;

/// Maximum score contribution from keyword density (per match, capped).
pub const KEYWORD_WEIGHT: f64 = 0.08;

/// Maximum score contribution from length normalization.
pub const LENGTH_WEIGHT: f64 = 0.30;

/// Maximum score contribution from memory-type bias.
pub const TYPE_WEIGHT: f64 = 0.40;

/// Base score applied to every memory before weighted contributions.
pub const BASE_SCORE: f64 = 0.10;

/// Ideal minimum length for a meaningful problem statement.
pub const MIN_IDEAL_LENGTH: usize = 16;

/// Ideal maximum length for a meaningful problem statement.
pub const MAX_IDEAL_LENGTH: usize = 400;

/// Cap on the number of keyword matches counted (prevents runaway scores).
pub const MAX_KEYWORD_MATCHES: usize = 6;

/// High-value keywords that signal important content.
///
/// Bilingual: an English-only table made `keyword_score` always 0 for Chinese
/// content while English collected up to 0.48 extra — combined with
/// `min_importance = 0.6` that systematically dropped Chinese memories.
const HIGH_VALUE_KEYWORDS: &[&str] = &[
    // English
    "error",
    "exception",
    "crash",
    "panic",
    "fix",
    "solve",
    "important",
    "critical",
    "security",
    "vulnerability",
    "deprecated",
    "migration",
    "production",
    "deployment",
    "config",
    "performance",
    "memory leak",
    "race condition",
    "deadlock",
    // Chinese
    "报错",
    "错误",
    "异常",
    "崩溃",
    "修复",
    "解决",
    "重要",
    "关键",
    "安全",
    "漏洞",
    "弃用",
    "迁移",
    "生产",
    "部署",
    "配置",
    "性能",
    "内存泄漏",
    "竞态",
    "死锁",
    "上线",
    "回滚",
];

/// Per-type baseline bias in `[0.0, 1.0]`.
///
/// Knowledge and Profile are long-lived and stable, so they get the highest
/// bias. Preference is medium. Interaction is the lowest because it decays
/// quickly and rarely encodes reusable knowledge.
fn type_bias(mt: MemoryType) -> f64 {
    match mt {
        // Long-lived, stable, highest reuse value.
        MemoryType::Knowledge | MemoryType::Skill => 0.95,
        // Stable identity facts.
        MemoryType::Profile => 0.85,
        // Situational lessons; useful but context-bound.
        MemoryType::Experience => 0.70,
        // User preferences; medium reuse value.
        MemoryType::Preference => 0.65,
        // Transient; decays quickly and rarely encodes reusable knowledge.
        MemoryType::Interaction => 0.40,
    }
}

/// Length-based score contribution.
///
/// Returns 0.0 when too short, ramps up linearly to `LENGTH_WEIGHT` at
/// `MIN_IDEAL_LENGTH`, stays at the maximum through the ideal range, then
/// ramps down linearly to 0.0 at `2 * MAX_IDEAL_LENGTH`. Beyond that, very
/// long messages are likely transcripts and score 0.
fn length_score(combined_len: usize) -> f64 {
    if combined_len < MIN_IDEAL_LENGTH {
        // Linear ramp-up from 0 at len=0 to LENGTH_WEIGHT at MIN_IDEAL_LENGTH.
        let ratio = combined_len as f64 / MIN_IDEAL_LENGTH as f64;
        return ratio * LENGTH_WEIGHT;
    }
    if combined_len <= MAX_IDEAL_LENGTH {
        return LENGTH_WEIGHT;
    }
    let overshoot = combined_len - MAX_IDEAL_LENGTH;
    let max_overshoot = MAX_IDEAL_LENGTH; // symmetric decay window
    if overshoot >= max_overshoot {
        return 0.0;
    }
    let decay_ratio = 1.0 - (overshoot as f64 / max_overshoot as f64);
    decay_ratio * LENGTH_WEIGHT
}

/// Keyword-density score contribution.
///
/// Counts case-insensitive substring matches of [`HIGH_VALUE_KEYWORDS`] in the
/// combined `problem + " " + solution` text, capped at
/// [`MAX_KEYWORD_MATCHES`]. Each match contributes [`KEYWORD_WEIGHT`].
fn keyword_score(combined_lower: &str) -> f64 {
    let matches = HIGH_VALUE_KEYWORDS
        .iter()
        .filter(|kw| combined_lower.contains(*kw))
        .count();
    let capped = matches.min(MAX_KEYWORD_MATCHES);
    capped as f64 * KEYWORD_WEIGHT
}

/// Stateless importance scorer.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImportanceScorer;

impl ImportanceScorer {
    /// Build a new scorer instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Score a `(problem, solution)` pair of a given [`MemoryType`].
    ///
    /// Returns a value clamped to `[0.0, 1.0]`. The score is the sum of:
    /// - [`BASE_SCORE`]
    /// - keyword-density contribution
    /// - length contribution
    /// - type-bias contribution (scaled by [`TYPE_WEIGHT`])
    ///
    /// # Arguments
    ///
    /// * `problem` - The distilled problem statement.
    /// * `solution` - The distilled solution / answer.
    /// * `mem_type` - The classification assigned by [`crate::classifier`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mnemosyne::scorer::ImportanceScorer;
    /// use mnemosyne::types::MemoryType;
    ///
    /// let s = ImportanceScorer::new();
    /// let score = s.score(
    ///     "How do I fix the memory leak?",
    ///     "Use RAII to manage the resource.",
    ///     MemoryType::Knowledge,
    /// );
    /// assert!(score > 0.5, "knowledge about a memory leak should score high");
    /// ```
    #[must_use]
    pub fn score(&self, problem: &str, solution: &str, mem_type: MemoryType) -> f64 {
        let combined = format!("{} {}", problem, solution);
        let combined_lower = combined.to_lowercase();

        let kw = keyword_score(&combined_lower);
        // Character count, not byte length: the ideal window (16..=400) is
        // reasoned about in characters. Byte length made a 6-char Chinese
        // problem (18 bytes) earn full LENGTH_WEIGHT while a 300-char Chinese
        // pair (~900 bytes) scored 0 and was silently dropped below
        // `min_importance`.
        let len = length_score(combined.chars().count());
        let type_score = type_bias(mem_type) * TYPE_WEIGHT;

        let raw = BASE_SCORE + kw + len + type_score;
        raw.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify knowledge about a critical error scores high.
    /// Invariants: Score should exceed 0.6 threshold for important content.
    #[test]
    fn score_critical_error_is_high() {
        let s = ImportanceScorer::new();
        let score = s.score(
            "How do I fix the memory leak in production?",
            "Use RAII to manage the resource and avoid the error.",
            MemoryType::Knowledge,
        );
        assert!(
            score > 0.6,
            "knowledge about memory leak + production + error should score > 0.6, got {score}"
        );
    }

    /// Objective: Verify very short content scores low.
    /// Invariants: Content below MIN_IDEAL_LENGTH scores below the length max.
    #[test]
    fn score_short_content_low_length_signal() {
        let s = ImportanceScorer::new();
        let short = s.score("ok", "sure", MemoryType::Interaction);
        assert!(
            short < 0.5,
            "very short interaction should score < 0.5, got {short}"
        );
    }

    /// Objective: Verify length-score ramp-up at short lengths.
    /// Invariants: length_score(0) == 0; length_score(MIN_IDEAL_LENGTH) == LENGTH_WEIGHT.
    #[test]
    fn length_score_ramp_up() {
        let zero = length_score(0);
        assert!(zero.abs() < f64::EPSILON, "len=0 should give 0");
        let ideal = length_score(MIN_IDEAL_LENGTH);
        assert!(
            (ideal - LENGTH_WEIGHT).abs() < f64::EPSILON,
            "len=MIN_IDEAL should give LENGTH_WEIGHT"
        );
        let half = length_score(MIN_IDEAL_LENGTH / 2);
        assert!(
            half > 0.0 && half < LENGTH_WEIGHT,
            "len=MIN_IDEAL/2 should give a partial score"
        );
    }

    /// Objective: Verify length-score decay beyond the ideal range.
    /// Invariants: length_score(MAX_IDEAL_LENGTH) == LENGTH_WEIGHT; decay to 0 at 2x.
    #[test]
    fn length_score_decay() {
        let max_ideal = length_score(MAX_IDEAL_LENGTH);
        assert!(
            (max_ideal - LENGTH_WEIGHT).abs() < f64::EPSILON,
            "len=MAX_IDEAL should give LENGTH_WEIGHT"
        );
        let doubled = length_score(MAX_IDEAL_LENGTH * 2);
        assert!(
            doubled.abs() < f64::EPSILON,
            "len=2*MAX_IDEAL should give 0"
        );
        let overshoot = length_score(MAX_IDEAL_LENGTH + MAX_IDEAL_LENGTH / 2);
        assert!(
            overshoot > 0.0 && overshoot < LENGTH_WEIGHT,
            "len between MAX_IDEAL and 2*MAX_IDEAL should give partial decay"
        );
    }

    /// Objective: Verify keyword score is capped at MAX_KEYWORD_MATCHES.
    /// Invariants: keyword_score never exceeds MAX_KEYWORD_MATCHES * KEYWORD_WEIGHT.
    #[test]
    fn keyword_score_cap() {
        // Stuff many high-value keywords into the input.
        let stuffed = "error exception crash panic fix solve important critical security";
        let score = keyword_score(stuffed);
        let cap = MAX_KEYWORD_MATCHES as f64 * KEYWORD_WEIGHT;
        assert!(
            score <= cap + f64::EPSILON,
            "keyword_score should not exceed cap, got {score} vs cap {cap}"
        );
    }

    /// Objective: Verify type bias ordering matches documentation.
    /// Invariants: Knowledge > Profile > Preference > Interaction.
    #[test]
    fn type_bias_ordering() {
        let k = type_bias(MemoryType::Knowledge);
        let p = type_bias(MemoryType::Profile);
        let pr = type_bias(MemoryType::Preference);
        let i = type_bias(MemoryType::Interaction);
        assert!(k > p, "Knowledge should beat Profile");
        assert!(p > pr, "Profile should beat Preference");
        assert!(pr > i, "Preference should beat Interaction");
    }

    /// Objective: Verify score is clamped to [0, 1].
    /// Invariants: Even pathological high-signal inputs stay <= 1.0.
    #[test]
    fn score_clamped_to_unit() {
        let s = ImportanceScorer::new();
        // Trigger all keywords at once with long content and Knowledge type.
        let mut text = String::new();
        for kw in HIGH_VALUE_KEYWORDS {
            text.push_str(kw);
            text.push(' ');
        }
        let score = s.score(&text, &text, MemoryType::Knowledge);
        assert!(score <= 1.0, "score must be clamped to <= 1.0, got {score}");
    }
}
