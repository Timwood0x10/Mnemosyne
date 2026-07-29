//! Resolver statistics — tracks hit rates for monitoring and debugging.
//!
//! After a compilation run, the stats can report:
//! - Alias hit rate (% of mentions resolved by exact match)
//! - Embedding hit rate (% resolved by vector search)
//! - Unknown rate (% that could not be resolved)
//! - Cache hit rate (% of embedding calls served from cache)
//!
//! A sudden drop in alias hit rate or spike in unknown rate is a strong
//! signal that the resolver configuration needs attention.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Thread-safe resolver statistics collector.
#[derive(Default)]
pub struct ResolverStats {
    pub total_mentions: AtomicUsize,
    pub alias_hit: AtomicUsize,
    pub embedding_hit: AtomicUsize,
    pub unknown: AtomicUsize,
    pub cache_hit: AtomicUsize,
    pub cache_miss: AtomicUsize,
}

impl ResolverStats {
    /// Create new zeroed stats.
    pub fn new() -> Self {
        ResolverStats::default()
    }

    /// Record one mention processed.
    pub fn record_mention(&self) {
        self.total_mentions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an alias hit.
    pub fn record_alias_hit(&self) {
        self.alias_hit.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an embedding hit.
    pub fn record_embedding_hit(&self) {
        self.embedding_hit.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an unknown (unresolved) mention.
    pub fn record_unknown(&self) {
        self.unknown.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache hit.
    pub fn record_cache_hit(&self) {
        self.cache_hit.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss.
    pub fn record_cache_miss(&self) {
        self.cache_miss.fetch_add(1, Ordering::Relaxed);
    }

    // ── Derived metrics ──────────────────────────────────────────────

    /// Percentage of mentions resolved via alias exact match.
    pub fn alias_hit_rate(&self) -> f64 {
        let total = self.total_mentions.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.alias_hit.load(Ordering::Relaxed) as f64 / total as f64 * 100.0
    }

    /// Percentage of mentions resolved via embedding vector search.
    pub fn embedding_hit_rate(&self) -> f64 {
        let total = self.total_mentions.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.embedding_hit.load(Ordering::Relaxed) as f64 / total as f64 * 100.0
    }

    /// Percentage of mentions that could not be resolved.
    pub fn unknown_rate(&self) -> f64 {
        let total = self.total_mentions.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.unknown.load(Ordering::Relaxed) as f64 / total as f64 * 100.0
    }

    /// Percentage of embedding calls served from cache.
    pub fn cache_hit_rate(&self) -> f64 {
        let hits = self.cache_hit.load(Ordering::Relaxed);
        let misses = self.cache_miss.load(Ordering::Relaxed);
        let total = hits + misses;
        if total == 0 {
            return 0.0;
        }
        hits as f64 / total as f64 * 100.0
    }

    /// Print a summary of all hit rates to stderr.
    pub fn print_report(&self) {
        eprintln!("Alias hit rate:   {:.1}%", self.alias_hit_rate());
        eprintln!("Embedding hit:    {:.1}%", self.embedding_hit_rate());
        eprintln!("Unknown:          {:.1}%", self.unknown_rate());
        eprintln!("Cache hit rate:   {:.1}%", self.cache_hit_rate());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that stats start at zero.
    /// Invariants: All rates are 0.0 for a fresh stats object.
    #[test]
    fn fresh_stats_are_zero() {
        let stats = ResolverStats::new();
        assert_eq!(stats.alias_hit_rate(), 0.0, "alias hit rate should be 0");
        assert_eq!(stats.unknown_rate(), 0.0, "unknown rate should be 0");
        assert_eq!(stats.cache_hit_rate(), 0.0, "cache hit rate should be 0");
    }

    /// Objective: Verify that recorded events produce correct percentages.
    /// Invariants: After 80 alias hits + 15 embedding hits + 5 unknown out of
    /// 100 mentions, rates are 80%, 15%, and 5% respectively.
    #[test]
    fn rates_reflect_recorded_events() {
        let stats = ResolverStats::new();
        for _ in 0..100 {
            stats.record_mention();
        }
        for _ in 0..80 {
            stats.record_alias_hit();
        }
        for _ in 0..15 {
            stats.record_embedding_hit();
        }
        for _ in 0..5 {
            stats.record_unknown();
        }

        assert!(
            (stats.alias_hit_rate() - 80.0).abs() < 0.01,
            "alias hit should be 80%"
        );
        assert!(
            (stats.embedding_hit_rate() - 15.0).abs() < 0.01,
            "embedding hit should be 15%"
        );
        assert!(
            (stats.unknown_rate() - 5.0).abs() < 0.01,
            "unknown should be 5%"
        );
    }

    /// Objective: Verify cache hit rate calculation.
    /// Invariants: 80 cache hits + 20 misses = 80% hit rate.
    #[test]
    fn cache_hit_rate() {
        let stats = ResolverStats::new();
        for _ in 0..80 {
            stats.record_cache_hit();
        }
        for _ in 0..20 {
            stats.record_cache_miss();
        }
        assert!(
            (stats.cache_hit_rate() - 80.0).abs() < 0.01,
            "cache hit should be 80%"
        );
    }
}
