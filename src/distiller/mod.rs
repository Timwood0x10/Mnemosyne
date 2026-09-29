//! Distillation pipeline orchestrator.
//!
//! [`Distiller`] runs the 8-stage pipeline that turns a slice of [`Message`]s
//! into a set of persisted [`Memory`] records.
//!
//! ## Pipeline stages
//!
//! 1. **Extract** — turn messages into [`RawExperience`] candidates.
//! 2. **Classify + Score + Filter** — assign [`MemoryType`], score
//!    importance, drop low-importance or noise candidates.
//! 3. **Top-N pre-filter** — keep the top `max_memories_per_distillation`
//!    candidates by importance.
//! 4. **Compress** — generate concise [`summary`](Memory::summary) from
//!    problem-solution pairs.
//! 5. **Embed** — generate embedding vectors via [`EmbeddingService`].
//! 6. **Conflict detection + resolution** — replace or keep based on
//!    similarity to existing memories.
//! 7. **Final Top-N** — re-rank after conflict resolution.
//! 8. **Capacity control** — enforce `max_solutions_per_tenant` per type.
//! 9. **Sync to store** — persist via [`ExperienceRepository`].

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::Mutex;

use crate::classifier::MemoryClassifier;
use crate::embed::EmbeddingService;
use crate::error::Result;
use crate::extractor::{ExperienceExtractor, ExtractorConfig, RawExperience};
use crate::filter::{NoiseFilter, SecurityFilter};
use crate::resolver::{ConflictResolver, Resolution};
use crate::scorer::ImportanceScorer;
use crate::store::ExperienceRepository;
use crate::types::{Experience, ExtractionMethod, Memory, MemoryType, Message, Metadata};

/// Configuration knobs for the distiller.
#[derive(Debug, Clone, Copy)]
pub struct DistillationConfig {
    /// Minimum importance to keep a memory; below this it is discarded.
    pub min_importance: f64,
    /// Cosine similarity above which two memories are considered conflicting.
    pub conflict_threshold: f64,
    /// Maximum memories produced per distillation call.
    pub max_memories_per_distillation: usize,
    /// Maximum solutions (Knowledge memories) per tenant.
    pub max_solutions_per_tenant: usize,
    /// Enable cross-turn extraction.
    pub enable_cross_turn: bool,
}

impl Default for DistillationConfig {
    fn default() -> Self {
        Self {
            min_importance: 0.6,
            conflict_threshold: 0.85,
            max_memories_per_distillation: 3,
            max_solutions_per_tenant: 5000,
            enable_cross_turn: true,
        }
    }
}

/// Mutable metrics counters tracked across distillation calls.
#[derive(Debug, Default)]
pub struct DistillationMetrics {
    attempts: AtomicU64,
    success: AtomicU64,
    failures: AtomicU64,
    filtered_noise: AtomicU64,
    filtered_security: AtomicU64,
    dropped_low_importance: AtomicU64,
    conflicts_resolved: AtomicU64,
    memories_created: AtomicU64,
    memories_replaced: AtomicU64,
    embed_calls: AtomicU64,
    embed_errors: AtomicU64,
    capacity_evictions: AtomicI64,
    memories_forgotten: AtomicU64,
}

impl DistillationMetrics {
    /// Build a fresh metrics container with all counters at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot all counters into a serializable form.
    ///
    /// Counters are read with relaxed ordering; the snapshot is a
    /// point-in-time view and may be slightly stale by the time it
    /// reaches the caller.
    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            attempts: self.attempts.load(Ordering::Relaxed),
            success: self.success.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            filtered_noise: self.filtered_noise.load(Ordering::Relaxed),
            filtered_security: self.filtered_security.load(Ordering::Relaxed),
            dropped_low_importance: self.dropped_low_importance.load(Ordering::Relaxed),
            conflicts_resolved: self.conflicts_resolved.load(Ordering::Relaxed),
            memories_created: self.memories_created.load(Ordering::Relaxed),
            memories_replaced: self.memories_replaced.load(Ordering::Relaxed),
            embed_calls: self.embed_calls.load(Ordering::Relaxed),
            embed_errors: self.embed_errors.load(Ordering::Relaxed),
            capacity_evictions: self.capacity_evictions.load(Ordering::Relaxed),
            memories_forgotten: self.memories_forgotten.load(Ordering::Relaxed),
        }
    }
}

/// Read-only point-in-time snapshot of [`DistillationMetrics`].
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MetricsSnapshot {
    /// Total distillation attempts (calls to [`Distiller::distill`]).
    pub attempts: u64,
    /// Attempts that completed without an infrastructure error.
    pub success: u64,
    /// Attempts that failed with an infrastructure error.
    pub failures: u64,
    /// Messages dropped by the noise filter.
    pub filtered_noise: u64,
    /// Messages dropped by the security filter.
    pub filtered_security: u64,
    /// Memories dropped because importance < `min_importance`.
    pub dropped_low_importance: u64,
    /// Existing memories replaced by newer conflicting ones.
    pub conflicts_resolved: u64,
    /// New memories created in the store.
    pub memories_created: u64,
    /// Memories superseded (replaced) by a newer one.
    pub memories_replaced: u64,
    /// Number of embedding service calls made.
    pub embed_calls: u64,
    /// Number of embedding service failures.
    pub embed_errors: u64,
    /// Net capacity-control evictions (negative = evictions).
    pub capacity_evictions: i64,
    /// Memories purged because their TTL expired (forget phase).
    pub memories_forgotten: u64,
}

/// Trait surface used by the MCP handlers; allows mocking in tests.
#[async_trait]
pub trait Distiller: Send + Sync {
    /// Run the full 8-stage distillation pipeline.
    async fn distill(
        &self,
        conversation_id: &str,
        messages: &[Message],
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Vec<Memory>>;

    /// Return a point-in-time snapshot of distillation metrics.
    fn metrics(&self) -> MetricsSnapshot;
}

/// Bounded LRU map of per-tenant distillation locks.
///
/// The map is capped at `MAX_ENTRIES`. When the cap is reached, the
/// oldest entry (least-recently-touched) is evicted. Active distillations
/// hold their own `Arc` clone, so eviction never interrupts an in-flight
/// distillation — it only allows a fresh lock to be created on the next
/// call from the evicted tenant.
///
/// Touch order is maintained with a [`std::collections::VecDeque`] front
/// index: `get_or_insert` moves the accessed key to the back, and
/// evictions pop from the front. This is O(n) per access but n is tiny
/// (capped at 1024), so the simplicity beats a linked-hash-map dependency.
struct LruTenantLocks {
    cap: usize,
    entries: std::collections::HashMap<String, Arc<Mutex<()>>>,
    order: std::collections::VecDeque<String>,
}

impl LruTenantLocks {
    /// Default cap on the number of tracked tenant locks.
    const MAX_ENTRIES: usize = 1024;

    /// Build a new bounded LRU map with the given capacity.
    #[must_use]
    fn new(cap: usize) -> Self {
        Self {
            cap,
            entries: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
        }
    }

    /// Get an existing lock, or insert a new one.
    ///
    /// On access the key is moved to the back of `order` (most-recently-used).
    /// When the map exceeds `cap`, the least-recently-used entry is dropped.
    fn get_or_insert(&mut self, key: &str) -> Arc<Mutex<()>> {
        // Fast path: existing entry, refresh LRU position.
        if let Some(arc) = self.entries.get(key) {
            let arc = arc.clone();
            self.order.retain(|k| k != key);
            self.order.push_back(key.to_string());
            return arc;
        }

        // Evict the least-recently-used entry if we are about to overflow.
        if self.entries.len() >= self.cap
            && let Some(evicted) = self.order.pop_front()
        {
            self.entries.remove(&evicted);
        }

        let arc = Arc::new(Mutex::new(()));
        self.entries.insert(key.to_string(), arc.clone());
        self.order.push_back(key.to_string());
        arc
    }
}

mod pipeline;
mod text;

pub use pipeline::PipelineDistiller;
pub use text::compress_pair;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
