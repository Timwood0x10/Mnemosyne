//! `PipelineDistiller`: the concrete 8-stage distillation orchestrator.

use super::text::content_hash;
use super::*;
use async_trait::async_trait;

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
                    // Non-fatal: a single embedding failure must not abort the
                    // whole distillation round. The memory keeps an empty
                    // vector and gracefully falls back to keyword-based
                    // conflict resolution downstream.
                    self.metrics.embed_errors.fetch_add(1, Ordering::Relaxed);
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
    /// Returns the memories worth storing **and** the ids they supersede.
    ///
    /// The superseded rows are NOT deleted here: a replacement is only a
    /// replacement once the new row is stored, so phase 8 deletes them inside
    /// the same transaction as the inserts (a failure in between used to lose
    /// the old memory without storing the new one).
    async fn phase_resolve_conflicts(
        &self,
        memories: Vec<Memory>,
    ) -> Result<(Vec<Memory>, Vec<String>)> {
        let mut kept = Vec::with_capacity(memories.len());
        let mut superseded: Vec<String> = Vec::new();
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
                    // Conflict candidates must belong to the SAME user: the
                    // vector search is tenant-wide, but another conversation's
                    // near-identical memory is not replaceable — deleting it
                    // (ReplaceOld below) would silently destroy that user's
                    // history (T15 cross-user conflict bug).
                    if exp.user_id != mem.user_id {
                        continue;
                    }
                    // Rehydrate the existing record into a Memory, then load
                    // its REAL stored embedding so the cosine comparison runs
                    // against genuine data. The raw `search_by_vector` results
                    // carry only the distance, not the vector, so we fetch the
                    // actual vector from the vec0 table via `get_vector`.
                    // (This is the fix for the bug where `existing_mem.vector`
                    // was set to the candidate's vector, making every similar
                    // neighbour compare as cosine == 1.0.)
                    let mut existing_mem = Memory::new(
                        &mem.tenant_id,
                        exp.memory_type,
                        &exp.content,
                        exp.confidence,
                    );
                    existing_mem.id = exp.id.clone();
                    existing_mem.user_id = exp.user_id.clone();
                    // Rehydrate the REAL stored vector. Propagate read
                    // failures: silently defaulting to an empty vector made a
                    // storage error look like "no embedding", so cosine
                    // similarity returned None, the resolver picked
                    // NoConflict, and a genuine duplicate was inserted.
                    existing_mem.vector = self.store.get_vector(&exp.id).await?;
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
                    superseded.push(old_id);
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
                content_hash(&exp.content) == new_hash
                    && exp.memory_type == mem.memory_type
                    // Same-user scope (T15): an identical content hash from
                    // ANOTHER user's conversation is not a duplicate of this
                    // candidate — deleting or dropping on it destroys one of
                    // the two histories.
                    && exp.user_id == mem.user_id
            });
            match duplicate {
                Some(exp) if mem.importance > exp.confidence => {
                    superseded.push(exp.id.clone());
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
        Ok((kept, superseded))
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
    /// lowest-confidence records are evicted; ties are broken oldest-first.
    ///
    /// # Scope
    ///
    /// The cap counts the tenant's `Knowledge` memories. This is a single-node
    /// MCP server, so the tenant is the local installation: the quota is simply
    /// "how many memories this installation keeps", and eviction takes the
    /// lowest-confidence rows whichever `user_id` wrote them.
    ///
    /// Conflict resolution is stricter on purpose and only ever replaces a row
    /// the same user wrote (T15). The difference is what is at stake: evicting
    /// a generic old row is housekeeping, while deleting somebody's only record
    /// of a decision is data loss. Every eviction is logged, so it is never
    /// invisible.
    ///
    /// `protected` holds the ids this round just created. They are excluded
    /// from the eviction candidates because eviction runs *after* the inserts,
    /// so the freshly-created rows are normally both the lowest-confidence and
    /// the newest: without the exclusion a round could delete its own output
    /// and still return it to the caller as a persisted memory (09-26/H9).
    ///
    /// `pub(super)` so the capacity tests can drive the phase directly while
    /// expired rows are still present — `distill` always purges them first, so
    /// the bug this guards is unreachable through `distill` alone.
    pub(super) async fn phase_enforce_capacity(
        &self,
        tenant_id: &str,
        protected: &std::collections::HashSet<String>,
    ) -> Result<()> {
        // Cheap pre-filter: the physical count is an indexed `COUNT` and bounds
        // the work, but it also counts TTL-expired rows the read path hides, so
        // it may only short-circuit — never determine how many rows to drop.
        let physical = self
            .store
            .count_by_memory_type(tenant_id, MemoryType::Knowledge)
            .await?;
        if physical as usize <= self.cfg.max_solutions_per_tenant {
            return Ok(());
        }
        // Fetch all Knowledge memories for the tenant, sort by
        // `(confidence asc, created_at asc)`, and delete the bottom `excess`
        // among the non-protected rows. Sorting oldest-first is what makes the
        // tie-break drop the OLDEST row: the query returns newest-first, and a
        // stable sort on confidence alone therefore evicted the newest member
        // of every tied group (09-26/H9).
        let all = self
            .store
            .get_by_memory_type(tenant_id, MemoryType::Knowledge)
            .await?;
        // Derive `excess` from the SAME list the victims come from. Counting
        // the physical rows instead made the cap look breached by expired rows
        // that can never be evicted, so the bottom `excess` slots spilled over
        // onto live rows that were actually under the cap.
        if all.len() <= self.cfg.max_solutions_per_tenant {
            return Ok(());
        }
        let excess = all.len() - self.cfg.max_solutions_per_tenant;
        let mut sorted: Vec<Experience> = all;
        sorted.sort_by(|a, b| {
            a.confidence
                .partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        let ids_to_delete: Vec<String> = sorted
            .iter()
            .filter(|e| !protected.contains(&e.id))
            .take(excess)
            .map(|e| e.id.clone())
            .collect();
        if !ids_to_delete.is_empty() {
            // Say what is about to disappear and why: an eviction the operator
            // only notices when a memory is missing is indistinguishable from a
            // bug.
            tracing::warn!(
                tenant = tenant_id,
                cap = self.cfg.max_solutions_per_tenant,
                evicted = ids_to_delete.len(),
                "capacity cap reached: evicting lowest-confidence memories"
            );
            self.store.delete_batch(&ids_to_delete).await?;
            self.metrics
                .capacity_evictions
                .fetch_sub(ids_to_delete.len() as i64, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Phase 8: persist memories and the rows they supersede, atomically.
    ///
    /// The deletes deferred by [`Self::phase_resolve_conflicts`] and the inserts
    /// go through one `replace_batch`, so a failure leaves the store exactly as
    /// it was: no lost update, and a "failed" distillation is not half applied.
    async fn phase_sync_to_store(&self, memories: &[Memory], superseded: &[String]) -> Result<()> {
        let mut replacements = Vec::with_capacity(memories.len());
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
            exp.expires_at = Some(mem.expires_at);
            exp.metadata = mem.metadata.clone();
            // Git-style summary (subject + causal body) rides in the metadata
            // bag since `Experience` has no dedicated summary column. Insert
            // AFTER the clone so it is not overwritten.
            exp.metadata
                .insert("summary", serde_json::Value::String(mem.summary.clone()));
            replacements.push(exp);
        }
        self.store.replace_batch(superseded, &replacements).await?;
        // Counted only after the writes land: these describe what happened,
        // not what was attempted.
        self.metrics
            .memories_created
            .fetch_add(replacements.len() as u64, Ordering::Relaxed);
        self.metrics
            .memories_replaced
            .fetch_add(superseded.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Phase 0 (maintenance): forget memories whose TTL has elapsed.
    ///
    /// This closes the memory lifecycle loop: extract → compress → conflict
    /// resolution → persist → **forget expired**. It runs as a tenant-level
    /// maintenance invariant — even when a distillation produces nothing.
    async fn phase_forget_expired(&self, tenant_id: &str) -> Result<usize> {
        let forgotten = self.store.forget_expired(tenant_id, Utc::now()).await?;
        if forgotten > 0 {
            self.metrics
                .memories_forgotten
                .fetch_add(forgotten as u64, Ordering::Relaxed);
        }
        Ok(forgotten)
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
            // Even with no new memories, maintenance must still fire: expiry
            // forgetting and capacity control are tenant-level invariants,
            // independent of whether this call produced anything.
            if let Err(e) = self.phase_forget_expired(tenant_id).await {
                self.metrics.failures.fetch_add(1, Ordering::Relaxed);
                return Err(e);
            }
            if let Err(e) = self
                .phase_enforce_capacity(tenant_id, &std::collections::HashSet::new())
                .await
            {
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
        let (memories, superseded) = match self.phase_resolve_conflicts(memories).await {
            Ok(resolved) => resolved,
            Err(e) => {
                self.metrics.failures.fetch_add(1, Ordering::Relaxed);
                return Err(e);
            }
        };

        // Phase 7: final top-N.
        let memories = self.phase_final_top_n(memories);

        // Phase 8: sync to store.
        if let Err(e) = self.phase_sync_to_store(&memories, &superseded).await {
            self.metrics.failures.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }

        // Maintenance: forget TTL-expired memories for this tenant. Runs in
        // the normal path too (not just the empty-messages branch) so active
        // tenants whose distillations keep producing memories still get their
        // expired rows purged instead of serving stale data indefinitely.
        if let Err(e) = self.phase_forget_expired(tenant_id).await {
            self.metrics.failures.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }

        // Phase 8 (continued): enforce capacity control after the new
        // records land, so the cap accounts for this distillation's output.
        // The rows this round just wrote are protected: eviction must never
        // delete the very memories we are about to return.
        let protected: std::collections::HashSet<String> =
            memories.iter().map(|m| m.id.clone()).collect();
        if let Err(e) = self.phase_enforce_capacity(tenant_id, &protected).await {
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
