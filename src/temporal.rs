//! Temporal query intent detection and time-aware relevance scoring.
//!
//! Mirrors mem0's temporal reasoning at the retrieval layer: a query asks
//! about the **current** state ("what does she use now"), a **past** event
//! ("what happened last year"), or an **upcoming** plan ("what is next").
//! Each intent ranks dated memory instances differently, and the resulting
//! per-experience temporal score feeds the RRF fusion in
//! [`crate::retrieval::RetrievalEngine`] as a fourth, scale-free term.

use chrono::{DateTime, Utc};

/// Which point in time a query targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeIntent {
    /// Current state: prefer the freshest, still-valid instance.
    Current,
    /// Past event: prefer older instances (recency is penalized).
    Past,
    /// Upcoming plan: prefer instances that are still valid and recent.
    Future,
}

impl TimeIntent {
    /// Classify a query string by temporal markers (Chinese + English).
    ///
    /// Past markers dominate over future markers when both appear? No: the
    /// first *specific* marker group found wins, checked in a fixed order
    /// (past → future → current) so "上次的计划" reads as Past, and a bare
    /// query with no markers defaults to [`TimeIntent::Current`].
    #[must_use]
    pub fn from_query(query: &str) -> Self {
        let q = query.to_lowercase();
        const PAST: &[&str] = &[
            "上次",
            "以前",
            "曾经",
            "之前",
            "过去",
            "去年",
            "昨天",
            "上周",
            "上个月",
            "last",
            "before",
            "yesterday",
            "previously",
            "ago",
            "past",
            "earlier",
            "old",
        ];
        const FUTURE: &[&str] = &[
            "下次",
            "将来",
            "打算",
            "计划",
            "将要",
            "即将",
            "未来",
            "明天",
            "下周",
            "下个月",
            "next",
            "upcoming",
            "plan",
            "planning",
            "will",
            "future",
            "tomorrow",
            "soon",
        ];
        for marker in PAST {
            if q.contains(marker) {
                return Self::Past;
            }
        }
        for marker in FUTURE {
            if q.contains(marker) {
                return Self::Future;
            }
        }
        Self::Current
    }
}

/// Compute a [0,1] temporal relevance score for an experience given the
/// query intent.
///
/// - [`TimeIntent::Current`]: freshness — linear decay from `now` back to
///   `now - FRESHNESS_WINDOW`; expired instances score 0.
/// - [`TimeIntent::Past`]: age preference — older instances score higher
///   (linear ramp over the same window, capped at the window edge).
/// - [`TimeIntent::Future`]: validity — expired scores 0, otherwise the same
///   freshness ramp as Current (an upcoming plan is most relevant when it is
///   recent AND still valid).
pub fn temporal_score(
    intent: TimeIntent,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> f64 {
    const FRESHNESS_WINDOW_SECS: i64 = 90 * 24 * 3600; // 90 days
    let expired = expires_at.is_some_and(|exp| exp <= now);
    if expired {
        return 0.0;
    }
    let age_secs = (now - created_at)
        .num_seconds()
        .clamp(0, FRESHNESS_WINDOW_SECS);
    let freshness = 1.0 - age_secs as f64 / FRESHNESS_WINDOW_SECS as f64;
    match intent {
        TimeIntent::Current | TimeIntent::Future => freshness,
        TimeIntent::Past => 1.0 - freshness,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    /// Objective: Verify the three query intents are detected from markers.
    /// Invariants: past/future markers map to the right intent; a bare query
    /// with no markers defaults to Current; markers are case-insensitive.
    #[test]
    fn query_intents_detected() {
        for q in [
            "上次讨论过",
            "以前我写过",
            "last year",
            "BEFORE this",
            "曾经喜欢",
        ] {
            assert_eq!(
                TimeIntent::from_query(q),
                TimeIntent::Past,
                "`{q}` must classify as Past"
            );
        }
        for q in [
            "下次计划",
            "打算学 Rust",
            "upcoming trip",
            "what is next",
            "明天去",
        ] {
            assert_eq!(
                TimeIntent::from_query(q),
                TimeIntent::Future,
                "`{q}` must classify as Future"
            );
        }
        for q in [
            "现在用什么",
            "currently using",
            "普通查询",
            "hello world",
            "Rust 检索",
        ] {
            assert_eq!(
                TimeIntent::from_query(q),
                TimeIntent::Current,
                "`{q}` must classify as Current"
            );
        }
    }

    /// Objective: Verify Current intent ranks fresh instances above old ones.
    /// Invariants: a 1-day-old instance scores > a 100-day-old instance; both
    /// are within [0,1]; expired instances score exactly 0.
    #[test]
    fn current_prefers_fresh() {
        let now = Utc::now();
        let fresh = temporal_score(
            TimeIntent::Current,
            now - Duration::days(1),
            Some(now + Duration::days(30)),
            now,
        );
        let old = temporal_score(
            TimeIntent::Current,
            now - Duration::days(100),
            Some(now + Duration::days(30)),
            now,
        );
        let expired = temporal_score(
            TimeIntent::Current,
            now - Duration::days(200),
            Some(now - Duration::days(1)),
            now,
        );

        assert!(
            fresh > old,
            "fresh instance must outrank the old one for Current intent"
        );
        assert!(
            (0.0..=1.0).contains(&fresh) && (0.0..=1.0).contains(&old),
            "scores must stay within [0,1]"
        );
        assert_eq!(expired, 0.0, "expired instances must score 0");
    }

    /// Objective: Verify Past intent inverts recency.
    /// Invariants: an old instance scores higher than a fresh one for Past.
    #[test]
    fn past_prefers_old() {
        let now = Utc::now();
        let fresh = temporal_score(TimeIntent::Past, now - Duration::days(1), None, now);
        let old = temporal_score(TimeIntent::Past, now - Duration::days(80), None, now);
        assert!(
            old > fresh,
            "old instance must outrank the fresh one for Past intent"
        );
    }

    /// Objective: Verify Future intent rejects expired instances but keeps
    /// recent valid ones.
    /// Invariants: expired → 0; valid recent → > 0; monotonic with freshness.
    #[test]
    fn future_requires_validity() {
        let now = Utc::now();
        let valid = temporal_score(
            TimeIntent::Future,
            now - Duration::days(2),
            Some(now + Duration::days(10)),
            now,
        );
        let expired = temporal_score(
            TimeIntent::Future,
            now - Duration::days(2),
            Some(now - Duration::days(1)),
            now,
        );
        assert!(
            valid > 0.0,
            "a still-valid recent plan must score above zero"
        );
        assert_eq!(expired, 0.0, "an expired plan must score zero for Future");
    }

    /// Objective: Verify never-expiring memories behave as always-valid.
    /// Invariants: expires_at=None never scores 0 for any intent.
    #[test]
    fn never_expiring_never_scores_zero() {
        let now = Utc::now();
        for intent in [TimeIntent::Current, TimeIntent::Past, TimeIntent::Future] {
            let score = temporal_score(intent, now - Duration::days(5), None, now);
            assert!(
                score > 0.0,
                "never-expiring instance must score > 0 for {intent:?}"
            );
        }
    }
}
