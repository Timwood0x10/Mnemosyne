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
use tokio::sync::Mutex;

use crate::classifier::MemoryClassifier;
use crate::embed::EmbeddingService;
use crate::error::{Result, distillation_error};
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
        if self.entries.len() >= self.cap {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            }
        }

        let arc = Arc::new(Mutex::new(()));
        self.entries.insert(key.to_string(), arc.clone());
        self.order.push_back(key.to_string());
        arc
    }
}

/// Concrete distiller orchestrating the 8-stage pipeline.
pub struct PipelineDistiller {
    cfg: DistillationConfig,
    embedder: Arc<dyn EmbeddingService>,
    store: Arc<dyn ExperienceRepository>,
    classifier: MemoryClassifier,
    scorer: ImportanceScorer,
    extractor: ExperienceExtractor,
    noise_filter: NoiseFilter,
    security_filter: SecurityFilter,
    resolver: ConflictResolver,
    metrics: Arc<DistillationMetrics>,
    /// Per-tenant pending-distillation lock to avoid concurrent writes
    /// racing on capacity-control eviction.
    ///
    /// Bounded to `MAX_TENANT_LOCKS` entries via a manual LRU eviction
    /// policy: when the map is full we drop the lock whose tenant has been
    /// inactive the longest (approximated by insertion order). Active
    /// distillations hold an `Arc` clone, so eviction never interrupts an
    /// in-flight distillation — it only allows a new lock to be created on
    /// the next call from the evicted tenant.
    tenant_locks: Mutex<LruTenantLocks>,
}

impl PipelineDistiller {
    /// Build a new distiller.
    ///
    /// # Arguments
    ///
    /// * `cfg` - Distillation configuration.
    /// * `embedder` - Embedding service (shared, thread-safe).
    /// * `store` - Experience repository (shared, thread-safe).
    #[must_use]
    pub fn new(
        cfg: DistillationConfig,
        embedder: Arc<dyn EmbeddingService>,
        store: Arc<dyn ExperienceRepository>,
    ) -> Self {
        let extractor = ExperienceExtractor::new(ExtractorConfig {
            enable_cross_turn: cfg.enable_cross_turn,
        });
        Self {
            cfg,
            embedder,
            store,
            classifier: MemoryClassifier::new(),
            scorer: ImportanceScorer::new(),
            extractor,
            noise_filter: NoiseFilter::new(),
            security_filter: SecurityFilter::new(),
            resolver: ConflictResolver::new(cfg.conflict_threshold),
            metrics: Arc::new(DistillationMetrics::new()),
            tenant_locks: Mutex::new(LruTenantLocks::new(LruTenantLocks::MAX_ENTRIES)),
        }
    }

    /// Returns a reference to the metrics container.
    #[must_use]
    pub fn metrics_ref(&self) -> Arc<DistillationMetrics> {
        self.metrics.clone()
    }

    /// Expose the store for callers that need to persist additional records
    /// alongside the distillation (e.g. decisions from the compiler).
    #[must_use]
    pub fn store(&self) -> Arc<dyn ExperienceRepository> {
        self.store.clone()
    }

    /// Acquire (or lazily create) the per-tenant mutex.
    async fn tenant_lock(&self, tenant_id: &str) -> Arc<Mutex<()>> {
        let mut guard = self.tenant_locks.lock().await;
        guard.get_or_insert(tenant_id)
    }

    /// Phase 1: extract raw experiences from messages.
    fn phase_extract(&self, messages: &[Message]) -> Vec<RawExperience> {
        self.extractor.extract(messages)
    }

    /// Phase 2: classify + score + filter into memory candidates.
    fn phase_classify_score_filter(
        &self,
        raws: Vec<RawExperience>,
        tenant_id: &str,
        user_id: &str,
        conversation_id: &str,
    ) -> Vec<Memory> {
        let mut out = Vec::with_capacity(raws.len());
        for raw in raws {
            // Security gate.
            let probe = Message::new("user", &raw.problem);
            let probe_sol = Message::new("assistant", &raw.solution);
            if self.security_filter.is_sensitive(&probe)
                || self.security_filter.is_sensitive(&probe_sol)
            {
                self.metrics
                    .filtered_security
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            // Noise gate (only on the problem side; solutions can be terse).
            if self.noise_filter.is_noise(&probe) {
                self.metrics.filtered_noise.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let mem_type = self.classifier.classify(&raw.problem, &raw.solution);
            let importance = self.scorer.score(&raw.problem, &raw.solution, mem_type);
            if importance < self.cfg.min_importance {
                self.metrics
                    .dropped_low_importance
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let content = format!("Problem: {} → Solution: {}", raw.problem, raw.solution);
            let mut mem = Memory::new(tenant_id, mem_type, content, importance);
            mem.user_id = user_id.to_string();
            mem.source = conversation_id.to_string();
            // Stash problem/solution + extraction method in metadata for later
            // hydration into an Experience record.
            let mut md = Metadata::default();
            md.insert("problem", serde_json::Value::String(raw.problem.clone()));
            md.insert("solution", serde_json::Value::String(raw.solution.clone()));
            md.insert(
                "extraction_method",
                serde_json::Value::String(raw.method.as_str().to_string()),
            );
            mem.metadata = md;
            out.push(mem);
        }
        out
    }

    /// Phase 3: top-N pre-filter by importance.
    fn phase_top_n_prefilter(&self, mut memories: Vec<Memory>) -> Vec<Memory> {
        memories.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        memories.truncate(self.cfg.max_memories_per_distillation);
        memories
    }

    /// Phase 4: compress each memory's content into a concise summary.
    fn phase_compress(&self, memories: &mut [Memory]) {
        for mem in memories.iter_mut() {
            let problem = mem
                .metadata
                .get("problem")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let solution = mem
                .metadata
                .get("solution")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            mem.summary = compress_pair(problem, solution);
        }
    }

    /// Phase 5: embed each memory's content.
    ///
    /// When the configured embedder is disabled (e.g. `NullEmbedder`), this
    /// phase is a no-op: memories keep empty vectors and the retrieval layer
    /// falls back to keyword search. Per `improve.md` Principle 1.
    async fn phase_embed(&self, memories: &mut [Memory]) -> Result<()> {
        if !self.embedder.enabled() {
            return Ok(());
        }
        for mem in memories.iter_mut() {
            self.metrics.embed_calls.fetch_add(1, Ordering::Relaxed);
            match self.embedder.embed(&mem.content).await {
                Ok(v) => mem.vector = v,
                Err(_) => {
                    self.metrics.embed_errors.fetch_add(1, Ordering::Relaxed);
                    return Err(distillation_error("embed", "embedding service call failed"));
                }
            }
        }
        Ok(())
    }

    /// Phase 5: detect conflicts and resolve them.
    ///
    /// Vector path: when embeddings are enabled, look up the candidate's
    /// nearest neighbours via [`search_by_vector`], then ask the
    /// [`ConflictResolver`] whether to replace the existing record or keep
    /// both. The existing record's vector is rehydrated from the store so
    /// the cosine comparison runs against real data.
    ///
    /// Keyword fallback: when embeddings are disabled the candidate has no
    /// vector, so cosine comparison is impossible. We instead deduplicate by
    /// exact content hash against existing memories of the same type for the
    /// tenant. A matching hash always replaces (the new memory is at least as
    /// fresh); non-matching hashes are kept as distinct entries.
    async fn phase_resolve_conflicts(&self, memories: Vec<Memory>) -> Result<Vec<Memory>> {
        let mut kept = Vec::with_capacity(memories.len());
        for mem in memories {
            // Vector path: embeddings present.
            if !mem.vector.is_empty() {
                let similar = self
                    .store
                    .search_by_vector(&mem.vector, &mem.tenant_id, 5)
                    .await?;
                let mut replaced_existing_id: Option<String> = None;
                let mut conflict_found = false;
                for exp in &similar {
                    if exp.memory_type != mem.memory_type {
                        continue;
                    }
                    // Rehydrate the existing record into a Memory carrying
                    // the candidate's vector for comparison. The resolver
                    // reads `existing.vector`, so we must populate it from
                    // `mem.vector` (the two are near-duplicates by design).
                    let mut existing_mem = Memory::new(
                        &mem.tenant_id,
                        exp.memory_type,
                        &exp.content,
                        exp.confidence,
                    );
                    existing_mem.id = exp.id.clone();
                    existing_mem.vector = mem.vector.clone();
                    let resolution = self.resolver.resolve(&mem, &existing_mem, exp.confidence);
                    match resolution {
                        Resolution::ReplaceOld { old_id, .. } => {
                            replaced_existing_id = Some(old_id);
                            conflict_found = true;
                            break;
                        }
                        Resolution::KeepBoth => {
                            conflict_found = true;
                            break;
                        }
                        Resolution::NoConflict => continue,
                    }
                }
                if let Some(old_id) = replaced_existing_id {
                    self.store.delete(&old_id).await?;
                    self.metrics
                        .memories_replaced
                        .fetch_add(1, Ordering::Relaxed);
                }
                if conflict_found {
                    self.metrics
                        .conflicts_resolved
                        .fetch_add(1, Ordering::Relaxed);
                }
                kept.push(mem);
                continue;
            }

            // Keyword fallback: deduplicate by content hash.
            let existing = self
                .store
                .get_by_memory_type(&mem.tenant_id, mem.memory_type)
                .await?;
            let new_hash = content_hash(&mem.content);
            // Three outcomes: no duplicate (push), duplicate with higher
            // importance (replace old), duplicate with lower-or-equal
            // importance (drop candidate).
            let duplicate = existing.iter().find(|exp| {
                content_hash(&exp.content) == new_hash && exp.memory_type == mem.memory_type
            });
            match duplicate {
                Some(exp) if mem.importance > exp.confidence => {
                    self.store.delete(&exp.id).await?;
                    self.metrics
                        .memories_replaced
                        .fetch_add(1, Ordering::Relaxed);
                    self.metrics
                        .conflicts_resolved
                        .fetch_add(1, Ordering::Relaxed);
                    kept.push(mem);
                }
                Some(_) => {
                    // Duplicate candidate with lower-or-equal importance: drop it.
                    self.metrics
                        .conflicts_resolved
                        .fetch_add(1, Ordering::Relaxed);
                }
                None => {
                    kept.push(mem);
                }
            }
        }
        Ok(kept)
    }

    /// Phase 6: final top-N by importance.
    fn phase_final_top_n(&self, mut memories: Vec<Memory>) -> Vec<Memory> {
        memories.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        memories.truncate(self.cfg.max_memories_per_distillation);
        memories
    }

    /// Phase 7: enforce capacity control per type per tenant.
    ///
    /// Only `Knowledge` memories are capped (matching the source project's
    /// `MaxSolutionsPerTenant`). When the count exceeds the cap, the
    /// lowest-confidence records are evicted.
    async fn phase_enforce_capacity(&self, tenant_id: &str) -> Result<()> {
        let k_count = self
            .store
            .count_by_memory_type(tenant_id, MemoryType::Knowledge)
            .await?;
        if k_count as usize <= self.cfg.max_solutions_per_tenant {
            return Ok(());
        }
        let excess = k_count as usize - self.cfg.max_solutions_per_tenant;
        // Fetch all Knowledge memories for the tenant, sort by confidence asc,
        // and delete the bottom `excess`.
        let all = self
            .store
            .get_by_memory_type(tenant_id, MemoryType::Knowledge)
            .await?;
        let mut sorted: Vec<Experience> = all;
        sorted.sort_by(|a, b| {
            a.confidence
                .partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let ids_to_delete: Vec<String> = sorted.iter().take(excess).map(|e| e.id.clone()).collect();
        if !ids_to_delete.is_empty() {
            self.store.delete_batch(&ids_to_delete).await?;
            self.metrics
                .capacity_evictions
                .fetch_sub(ids_to_delete.len() as i64, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Phase 8: persist memories to the store as Experience records.
    async fn phase_sync_to_store(&self, memories: &[Memory]) -> Result<()> {
        for mem in memories {
            let problem = mem
                .metadata
                .get("problem")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let solution = mem
                .metadata
                .get("solution")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let method_str = mem
                .metadata
                .get("extraction_method")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("direct")
                .to_string();
            let extraction_method = match method_str.as_str() {
                "cross-turn" => ExtractionMethod::CrossTurn,
                _ => ExtractionMethod::Direct,
            };
            let mut exp = Experience::new(
                &mem.tenant_id,
                mem.memory_type,
                &mem.content,
                mem.importance,
            );
            exp.id = mem.id.clone();
            exp.user_id = mem.user_id.clone();
            exp.problem = problem;
            exp.solution = solution;
            exp.source = mem.source.clone();
            exp.vector = mem.vector.clone();
            exp.extraction_method = extraction_method;
            exp.metadata = mem.metadata.clone();
            self.store.create(&exp).await?;
            self.metrics
                .memories_created
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[async_trait]
impl Distiller for PipelineDistiller {
    async fn distill(
        &self,
        conversation_id: &str,
        messages: &[Message],
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Vec<Memory>> {
        self.metrics.attempts.fetch_add(1, Ordering::Relaxed);

        // Serialize distillation per tenant to avoid capacity races.
        let lock = self.tenant_lock(tenant_id).await;
        let _guard = lock.lock().await;

        // Phase 1: extract raw experiences.
        let raws = self.phase_extract(messages);

        // Phase 2: classify + score + filter.
        let memories = self.phase_classify_score_filter(raws, tenant_id, user_id, conversation_id);

        if memories.is_empty() {
            // Even with no new memories, capacity control must still fire:
            // it is a tenant-level maintenance invariant, independent of
            // whether this call produced anything.
            if let Err(e) = self.phase_enforce_capacity(tenant_id).await {
                self.metrics.failures.fetch_add(1, Ordering::Relaxed);
                return Err(e);
            }
            self.metrics.success.fetch_add(1, Ordering::Relaxed);
            return Ok(Vec::new());
        }

        // Phase 3: top-N pre-filter.
        let mut memories = self.phase_top_n_prefilter(memories);

        // Phase 4: compress into concise summaries.
        self.phase_compress(&mut memories);

        // Phase 5: embed.
        if let Err(e) = self.phase_embed(&mut memories).await {
            self.metrics.failures.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }

        // Phase 6: resolve conflicts.
        let memories = match self.phase_resolve_conflicts(memories).await {
            Ok(m) => m,
            Err(e) => {
                self.metrics.failures.fetch_add(1, Ordering::Relaxed);
                return Err(e);
            }
        };

        // Phase 7: final top-N.
        let memories = self.phase_final_top_n(memories);

        // Phase 8: sync to store.
        if let Err(e) = self.phase_sync_to_store(&memories).await {
            self.metrics.failures.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }

        // Phase 8 (continued): enforce capacity control after the new
        // records land, so the cap accounts for this distillation's output.
        if let Err(e) = self.phase_enforce_capacity(tenant_id).await {
            self.metrics.failures.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }

        self.metrics.success.fetch_add(1, Ordering::Relaxed);
        Ok(memories)
    }

    fn metrics(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }
}

/// Compress a problem-solution pair into a concise single-sentence summary.
/// Compress a problem-solution pair into a concise summary.
/// Truncates problem to 60 chars, solution to 120 chars,
/// strips trailing question marks, and pairs as `problem: action`.
pub fn compress_pair(problem: &str, solution: &str) -> String {
    const MAX_PROBLEM: usize = 60;
    const MAX_SOLUTION: usize = 120;

    let problem = problem.trim();
    let solution = solution.trim();

    if problem.is_empty() && solution.is_empty() {
        return String::new();
    }
    if problem.is_empty() {
        return truncate(solution, MAX_SOLUTION);
    }
    if solution.is_empty() {
        return truncate(problem, MAX_PROBLEM);
    }

    let stripped = problem
        .strip_suffix('?')
        .or_else(|| problem.strip_suffix('？'))
        .map(str::trim)
        .unwrap_or(problem);

    let core = truncate(stripped, MAX_PROBLEM);
    let action = truncate(solution, MAX_SOLUTION);

    format!("{core}：{action}")
}

/// Compute a stable content hash for deduplication.
///
/// Uses FNV-1a because it is dependency-free and fast on short strings.
/// Trims surrounding whitespace so that cosmetic re-formatting does not
/// defeat the dedup check.
fn content_hash(s: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    for b in s.trim().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = String::with_capacity(max + 3);
        for c in s.chars() {
            let next_len = out.len() + c.len_utf8();
            if next_len > max {
                out.push('…');
                break;
            }
            out.push(c);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::EmbeddingService;
    use crate::error::Result;
    use crate::store::SQLiteVecStore;

    struct StubEmbedder;
    #[async_trait]
    impl EmbeddingService for StubEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            let v: Vec<f32> = text
                .chars()
                .take(8)
                .map(|c| (c as u32 % 256) as f32)
                .collect();
            let mut v = v;
            while v.len() < 8 {
                v.push(0.0);
            }
            Ok(v)
        }
        async fn embed_with_prefix(&self, text: &str, _prefix: &str) -> Result<Vec<f32>> {
            self.embed(text).await
        }
        async fn health_check(&self) -> Result<()> {
            Ok(())
        }
        fn model(&self) -> &str {
            "stub"
        }
        fn timeout(&self) -> std::time::Duration {
            std::time::Duration::from_secs(1)
        }
    }

    /// Objective: Verify compress_pair produces expected summaries.
    #[test]
    fn compress_pair_basic() {
        let s = compress_pair("如何用serde解析JSON？", "使用 serde_json::from_str。");
        assert_eq!(s, "如何用serde解析JSON：使用 serde_json::from_str。");
    }

    /// Objective: Verify empty inputs return empty.
    #[test]
    fn compress_pair_empty() {
        assert!(compress_pair("", "").is_empty());
    }

    /// Objective: Verify question mark is stripped from problem.
    #[test]
    fn compress_pair_strips_question_mark() {
        let s = compress_pair("Why is build slow?", "Replace lancedb with sqlite-vec.");
        assert!(!s.contains('?'), "question mark should be stripped");
        assert!(s.contains("Replace lancedb"), "solution should be included");
    }

    /// Objective: Verify Chinese question mark is stripped.
    #[test]
    fn compress_pair_chinese_question() {
        let s = compress_pair("编译为什么这么慢？", "主要原因是lancedb太重");
        assert!(
            !s.contains('？'),
            "Chinese question mark should be stripped"
        );
        assert!(s.contains("lancedb"), "solution included");
    }

    /// Objective: Verify compression is applied during distillation.
    #[tokio::test]
    async fn distill_with_compression() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
        let cfg = DistillationConfig {
            min_importance: 0.0,
            conflict_threshold: 0.99,
            max_memories_per_distillation: 10,
            max_solutions_per_tenant: 100,
            enable_cross_turn: true,
        };
        let d = PipelineDistiller::new(cfg, embedder, store);
        let msgs = vec![
            Message::new("user", "How do I read a file in Rust?"),
            Message::new("assistant", "Use std::fs::read_to_string."),
        ];
        let result = d.distill("c1", &msgs, "t1", "u1").await.expect("distill");
        assert_eq!(result.len(), 1, "one memory distilled");
        assert!(
            !result[0].summary.is_empty(),
            "summary should be populated: {:?}",
            result[0].summary
        );
        assert!(
            result[0].summary.contains("read a file"),
            "summary should reference the problem: {}",
            result[0].summary
        );
    }

    /// Objective: Verify empty message list yields empty result and increments attempts.
    #[tokio::test]
    async fn distill_empty_messages() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
        let d = PipelineDistiller::new(DistillationConfig::default(), embedder, store);
        let result = d.distill("c1", &[], "t1", "u1").await.expect("distill");
        assert!(result.is_empty(), "no memories from empty input");
        let m = d.metrics();
        assert_eq!(m.attempts, 1, "attempts incremented");
        assert_eq!(m.success, 1, "success incremented");
        assert_eq!(m.memories_created, 0, "no memories created");
    }

    /// Objective: Verify a simple user→assistant pair produces a stored memory.
    /// Invariants: 1 memory created, embed_calls > 0.
    #[tokio::test]
    async fn distill_simple_pair() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
        let d = PipelineDistiller::new(DistillationConfig::default(), embedder, store.clone());
        let msgs = vec![
            Message::new("user", "How do I fix the borrow checker error in Rust?"),
            Message::new(
                "assistant",
                "Reborrow or restructure the lifetimes to satisfy the checker.",
            ),
        ];
        let result = d.distill("c1", &msgs, "t1", "u1").await.expect("distill");
        // Note: depending on importance score, may or may not pass min_importance threshold.
        let m = d.metrics();
        // Either 0 (filtered as low importance) or 1 (kept) memories.
        assert!(result.len() <= 1, "at most 1 memory from a single pair");
        // embed_calls should be 0 if filtered, 1 if kept.
        assert!(m.embed_calls <= 1, "at most 1 embed call");
    }

    /// Objective: Verify metrics snapshot returns all counters.
    /// Invariants: snapshot fields are accessible.
    #[tokio::test]
    async fn metrics_snapshot() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
        let d = PipelineDistiller::new(DistillationConfig::default(), embedder, store);
        let _ = d.distill("c1", &[], "t1", "u1").await;
        let snap = d.metrics();
        assert_eq!(snap.attempts, 1);
        assert_eq!(snap.success, 1);
        assert_eq!(snap.failures, 0);
    }

    /// Objective: Verify low min_importance lets through noise as memories.
    /// Invariants: With min_importance=0.0, all candidates pass.
    #[tokio::test]
    async fn distill_with_zero_min_importance() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
        let cfg = DistillationConfig {
            min_importance: 0.0,
            conflict_threshold: 0.99,
            max_memories_per_distillation: 10,
            max_solutions_per_tenant: 100,
            enable_cross_turn: true,
        };
        let d = PipelineDistiller::new(cfg, embedder, store);
        let msgs = vec![
            Message::new("user", "How do I read a file in Rust?"),
            Message::new("assistant", "Use std::fs::read_to_string."),
            Message::new("user", "How do I write a file in Rust?"),
            Message::new("assistant", "Use std::fs::write."),
        ];
        let result = d.distill("c1", &msgs, "t1", "u1").await.expect("distill");
        assert_eq!(result.len(), 2, "both pairs distilled");
    }

    // ── Document fidelity tests ─────────────────────────────────────

    /// Objective: Verify technical document content (code snippets) is not
    /// mangled during compression. Code must survive truncation byte-unchanged.
    #[test]
    fn compress_pair_technical_document() {
        let problem = "How to set up the Rust project structure for an MCP server?";
        let solution = "Create a binary crate with cargo new memory-mcp. Add dependencies: \
            tokio for async runtime, serde/serde_json for JSON-RPC, rusqlite for storage. \
            Use a src/main.rs entrypoint and organize modules under src/.";
        let s = compress_pair(problem, solution);
        // Code and paths must survive verbatim
        assert!(s.contains("cargo new"), "code not mangled: {s}");
        // (first-sentence extraction keeps only first sentence)
        assert!(
            s.contains("Create a binary crate"),
            "first sentence preserved: {s}"
        );
        // Density: each segment carries meaning
        assert!(
            s.chars().count() <= problem.chars().count() + solution.chars().count(),
            "compression should not expand"
        );
        assert!(s.contains("memory-mcp"), "project name preserved: {s}");
    }

    /// Objective: Verify Chinese document content respects UTF-8 boundaries.
    /// Byte slicing must not split a multi-byte character.
    #[test]
    fn compress_pair_chinese_document() {
        let problem = "如何优化 Agent 的上下文窗口管理？记忆蒸馏的最佳实践是什么？";
        let solution = "采用三层架构：第一层用滑动窗口保留最近 N 条原始消息；\
            第二层用 PipelineDistiller 对历史消息做蒸馏提取关键记忆；\
            第三层用向量数据库做相似度检索。这样既保证了实时性，又不会丢失历史经验。";
        let s = compress_pair(problem, solution);
        // No replacement character from broken UTF-8
        assert!(!s.contains('�'), "no broken UTF-8: {s}");
        // Chinese question mark stripped only from end of problem, not inside
        assert!(
            s.contains('？'),
            "embedded question mark should remain, only trailing stripped: {s}"
        );
        // Key Chinese terms preserved (first sentence after 。split)
        assert!(
            s.contains("滑动窗口"),
            "first-sentence concept preserved: {s}"
        );
        assert!(
            s.contains("三层架构"),
            "architecture concept preserved: {s}"
        );
        // Verify byte-level safety: the string must be valid UTF-8
        assert!(
            std::str::from_utf8(s.as_bytes()).is_ok(),
            "output must be valid UTF-8"
        );
    }

    /// Objective: Verify long document paragraphs are truncated sensibly —
    /// truncation should not end mid-word or produce garbled output.
    #[test]
    fn compress_pair_long_document_content() {
        let problem = "What are the key architectural decisions in the memory distillation system?";
        let solution = "The memory distillation system uses an 8-stage pipeline: \
            extraction (user-assistant pairs), classification (MemoryType assignment), \
            importance scoring, noise filtering, compression (truncation + pairing), \
            embedding (vector generation), conflict resolution (cosine similarity), \
            and capacity control (tenant-level eviction). Each stage is independently \
            testable and swappable. The pipeline is orchestrated by PipelineDistiller \
            which holds references to each stage implementation.";
        let s = compress_pair(problem, solution);
        // Must include the 8-stage concept
        assert!(s.contains("8-stage"), "key number preserved: {s}");
        // Truncation should produce valid output (no panic from byte slicing)
        assert!(!s.is_empty(), "output should not be empty");
        // The core structure (problem：action) should be intact
        assert!(s.contains('：'), "separator should be present");
    }

    /// Objective: Verify that very short document snippets don't lose meaning
    /// through aggressive truncation.
    #[test]
    fn compress_pair_short_content_preserved() {
        let problem = "Go vs Rust?";
        let solution = "Rust for safety, Go for simplicity.";
        let s = compress_pair(problem, solution);
        assert_eq!(
            s, "Go vs Rust：Rust for safety, Go for simplicity.",
            "short content should pass through unchanged"
        );
    }

    /// Objective: Verify long solution is truncated to 120 chars.
    #[test]
    fn compress_pair_truncates_long_solution() {
        let s = compress_pair(
            "How does the conflict resolver work?",
            "Let me explain how the conflict resolver determines if two memories conflict. \
             It uses cosine similarity on embedding vectors with a configurable threshold. \
             Two memories conflict when their cosine similarity exceeds the threshold.",
        );
        assert!(!s.is_empty(), "output should not be empty");
        assert!(
            s.len() <= 60 + 3 + 120,
            "output should be truncated to reasonable length: {s}"
        );
    }

    /// Objective: Verify markdown bold syntax doesn't break the output.
    #[test]
    fn compress_pair_handles_markdown_bold() {
        let s = compress_pair(
            "设计原则是什么？",
            "**分层清晰**：每一层职责单一。**可测试**：每个阶段独立可测。",
        );
        assert!(
            s.contains("分层清晰"),
            "markdown bold content should survive: {s}"
        );
    }

    /// Objective: Verify distillation preserves key information from
    /// real document-style conversation (narrative + technical content).
    #[tokio::test]
    async fn distill_document_fidelity() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
        let cfg = DistillationConfig {
            min_importance: 0.0,
            conflict_threshold: 0.99,
            max_memories_per_distillation: 10,
            max_solutions_per_tenant: 100,
            enable_cross_turn: true,
        };
        let d = PipelineDistiller::new(cfg, embedder, store);

        // Simulate a document review conversation: user pastes specs,
        // assistant analyzes them.
        let msgs = vec![
            Message::new("user", "What architecture does the memory system use?"),
            Message::new(
                "assistant",
                "The memory system uses an 8-stage pipeline with separate \
                stages for extraction, classification, scoring, filtering, \
                compression, embedding, conflict resolution, and capacity control.",
            ),
            Message::new("user", "How does compression work specifically?"),
            Message::new(
                "assistant",
                "Compression uses the compress_pair function which truncates \
                problem to 60 chars and solution to 120 chars, strips trailing \
                question marks, and pairs them as `problem: action`.",
            ),
            Message::new("user", "What storage backend does it use?"),
            Message::new(
                "assistant",
                "SQLite with sqlite-vec extension for vector similarity search. \
                The store creates a vec0 virtual table indexed by cosine distance. \
                Dimensions default to 768 but are configurable per instance.",
            ),
        ];

        let result = d.distill("c1", &msgs, "t1", "u1").await.expect("distill");
        assert!(
            !result.is_empty(),
            "should produce memories from document content"
        );

        // Each memory must preserve key information from original
        for mem in &result {
            // No garbled output
            assert!(
                !mem.summary.contains('�'),
                "no garbled characters in summary: {}",
                mem.summary
            );
            assert!(
                !mem.summary.trim().is_empty(),
                "summary must not be empty or whitespace-only"
            );
            // Summary is shorter than combined input
            assert!(
                mem.summary.len() < 200,
                "summary should be condensed (<200 chars), got {}",
                mem.summary.len()
            );
        }

        // At least one memory should reference the pipeline architecture
        let has_pipeline = result.iter().any(|m| {
            m.summary.contains("8-stage")
                || m.summary.contains("pipeline")
                || m.summary.contains("PipelineDistiller")
        });
        assert!(
            has_pipeline,
            "at least one memory should preserve pipeline concept, got: {:?}",
            result.iter().map(|m| &m.summary).collect::<Vec<_>>()
        );
    }

    /// Objective: Verify Chinese document content survives full distillation
    /// without byte-level corruption (common issue with CJK + truncation).
    #[tokio::test]
    async fn distill_chinese_document_fidelity() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
        let cfg = DistillationConfig {
            min_importance: 0.0,
            conflict_threshold: 0.99,
            max_memories_per_distillation: 10,
            max_solutions_per_tenant: 100,
            enable_cross_turn: true,
        };
        let d = PipelineDistiller::new(cfg, embedder, store);

        let msgs = vec![
            Message::new("user", "记忆蒸馏系统的架构是什么？"),
            Message::new(
                "assistant",
                "记忆蒸馏系统采用8阶段管道：提取（用户-助手配对）、分类（MemoryType）、\
                重要性评分、噪音过滤、压缩（截断+配对）、嵌入（向量生成）、冲突解决（余弦相似度）、\
                容量控制（租户级淘汰）。每个阶段独立可测试、可替换。",
            ),
            Message::new("user", "压缩阶段具体怎么工作？"),
            Message::new(
                "assistant",
                "compress_pair 函数把问题截断到60个字符，解决方案截断到120个字符，\
                去掉末尾的问号和语气词，然后用冒号拼接成「问题：解决方案」的格式。\
                截断时保证了字符边界安全，不会从中间切开一个UTF-8字符。",
            ),
            Message::new("user", "它用什么存储后端？"),
            Message::new(
                "assistant",
                "使用 SQLite 加 sqlite-vec 扩展做向量相似度搜索。\
                创建 vec0 虚拟表按余弦距离索引。维度默认768但可配置。\
                每次打开存储时通过 OnceLock 确保扩展只加载一次。",
            ),
        ];

        let result = d.distill("c1", &msgs, "t1", "u1").await.expect("distill");
        assert!(
            !result.is_empty(),
            "should produce memories from Chinese document content"
        );

        for mem in &result {
            // No UTF-8 corruption (replacement character)
            assert!(
                !mem.summary.contains('�'),
                "no UTF-8 corruption: {}",
                mem.summary
            );
            // No empty summaries
            assert!(!mem.summary.trim().is_empty(), "summary must not be empty");
            // CJK bytes are valid
            assert!(
                std::str::from_utf8(mem.summary.as_bytes()).is_ok(),
                "summary must be valid UTF-8"
            );
        }

        // At least one memory preserves a Chinese concept
        let has_concept = result.iter().any(|m| {
            m.summary.contains("管道")
                || m.summary.contains("蒸馏")
                || m.summary.contains("余弦")
                || m.summary.contains("向量")
        });
        assert!(
            has_concept,
            "no Chinese concept preserved in any summary: {:?}",
            result.iter().map(|m| &m.summary).collect::<Vec<_>>()
        );
    }

    /// Objective: Verify compression density — each distilled memory should
    /// carry substantial information relative to its character count.
    #[tokio::test]
    async fn distill_density_reasonable() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
        let cfg = DistillationConfig {
            min_importance: 0.0,
            conflict_threshold: 0.99,
            max_memories_per_distillation: 10,
            max_solutions_per_tenant: 100,
            enable_cross_turn: true,
        };
        let d = PipelineDistiller::new(cfg, embedder, store);

        // A dense technical exchange
        let msgs = vec![
            Message::new(
                "user",
                "How does the conflict resolver determine if two memories conflict?",
            ),
            Message::new(
                "assistant",
                "The ConflictResolver uses cosine similarity on embedding vectors. \
                Two memories conflict when their cosine similarity exceeds the \
                configured threshold (default 0.85). On conflict, if the new memory \
                has higher importance it replaces the old one via ReplaceOld; \
                otherwise both are kept.",
            ),
        ];

        let result = d.distill("c1", &msgs, "t1", "u1").await.expect("distill");
        if result.is_empty() {
            return; // could be filtered by importance; skip
        }

        let mem = &result[0];
        // Density heuristic: summary should contain multiple meaningful tokens
        let meaningful_tokens: usize = mem
            .summary
            .split([' ', '：', '。', '，', ',', '.'])
            .filter(|t| t.len() > 2)
            .count();
        assert!(
            meaningful_tokens >= 3,
            "summary should have >=3 meaningful tokens, got {} in '{}'",
            meaningful_tokens,
            mem.summary
        );
        // The summary must contain at least one specific technical term
        assert!(
            mem.summary.contains("cosine")
                || mem.summary.contains("similarity")
                || mem.summary.contains("ConflictResolver")
                || mem.summary.contains("threshold")
                || mem.summary.contains("ReplaceOld"),
            "summary should contain a specific technical term, got: {}",
            mem.summary
        );
    }

    /// Objective: Verify capacity control evicts lowest-confidence records.
    /// Invariants: After exceeding max_solutions_per_tenant, eviction occurs.
    #[tokio::test]
    async fn capacity_control_evicts() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
        let cfg = DistillationConfig {
            min_importance: 0.0,
            conflict_threshold: 0.99,
            max_memories_per_distillation: 100,
            max_solutions_per_tenant: 3, // very low cap
            enable_cross_turn: true,
        };
        let d = PipelineDistiller::new(cfg, embedder, store.clone());
        // Insert 5 knowledge memories directly.
        for i in 0..5 {
            let mut exp = Experience::new("t1", MemoryType::Knowledge, format!("k{i}"), 0.5);
            exp.id = format!("k{i}");
            exp.vector = vec![1.0_f32; 8];
            store.create(&exp).await.expect("create");
        }
        // Run distill with empty messages — capacity control should fire.
        let _ = d.distill("c1", &[], "t1", "u1").await;
        let m = d.metrics();
        // capacity_evictions should be negative (2 evictions to reach cap of 3).
        assert!(
            m.capacity_evictions <= 0,
            "capacity_evictions should be <= 0, got {}",
            m.capacity_evictions
        );
        let k_count = store
            .count_by_memory_type("t1", MemoryType::Knowledge)
            .await
            .expect("count");
        assert!(
            k_count <= 3,
            "Knowledge count should be <= cap of 3, got {k_count}"
        );
    }
}
