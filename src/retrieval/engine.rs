//! `RetrievalEngine` implementation: keyword / vector / hybrid search.

use super::*;

impl RetrievalEngine {
    /// Build a new retrieval engine.
    ///
    /// # Arguments
    ///
    /// * `embedder` - Embedding service used to embed queries in vector/hybrid
    ///   mode. May be a `NullEmbedder` when running keyword-only.
    /// * `store` - Experience repository for vector search and candidate
    ///   fetching.
    /// * `mode` - Retrieval mode (keyword/vector/hybrid).
    #[must_use]
    pub fn new(
        embedder: Arc<dyn EmbeddingService>,
        store: Arc<dyn ExperienceRepository>,
        mode: RetrievalMode,
    ) -> Self {
        Self {
            embedder,
            store,
            mode,
            external: None,
        }
    }

    /// Attach an external-knowledge registry for hybrid external-signal fusion.
    ///
    /// When attached, [`RetrievalMode::Hybrid`] search forwards the query to
    /// every registered signal provider and fuses the hits into RRF as
    /// synthetic candidates (external-knowledge-plan §B4). Other modes are
    /// unaffected. The registry is shared by `Arc`, so the same instance can
    /// also drive `knowledge_attach`/`knowledge_ingest` from the MCP layer.
    #[must_use]
    pub fn with_external_registry(mut self, registry: Arc<ExternalKnowledgeRegistry>) -> Self {
        self.external = Some(registry);
        self
    }

    /// Replace the external-knowledge registry at runtime.
    ///
    /// Used by the `knowledge_attach` MCP tool so newly-attached sources are
    /// visible to subsequent `memory_search` calls without rebuilding the
    /// engine.
    pub fn set_external_registry(&mut self, registry: Arc<ExternalKnowledgeRegistry>) {
        self.external = Some(registry);
    }

    /// Read-only access to the attached external registry, if any.
    #[must_use]
    pub fn external_registry(&self) -> Option<&Arc<ExternalKnowledgeRegistry>> {
        self.external.as_ref()
    }

    /// Returns the configured retrieval mode.
    #[must_use]
    pub fn mode(&self) -> RetrievalMode {
        self.mode
    }

    /// Execute a retrieval query.
    ///
    /// # Arguments
    ///
    /// * `query` - Search query text.
    /// * `tenant_id` - Tenant scope.
    /// * `limit` - Maximum number of results to return.
    /// * `memory_type_filter` - Optional memory type filter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Embedding`] if query embedding fails in vector/hybrid
    /// mode, or [`Error::Storage`] if the underlying store search fails.
    pub async fn search(
        &self,
        query: &str,
        tenant_id: &str,
        limit: usize,
        memory_type_filter: Option<MemoryType>,
    ) -> Result<Vec<RetrievalResult>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        match self.mode {
            RetrievalMode::Keyword => {
                self.keyword_search(query, tenant_id, limit, memory_type_filter)
                    .await
            }
            RetrievalMode::Vector => {
                self.vector_search(query, tenant_id, limit, memory_type_filter)
                    .await
            }
            RetrievalMode::Hybrid => {
                self.hybrid_search(query, tenant_id, limit, memory_type_filter)
                    .await
            }
        }
    }

    /// Keyword-only search: delegates to store's FTS5 or BM25 full-scan.
    async fn keyword_search(
        &self,
        query: &str,
        tenant_id: &str,
        limit: usize,
        memory_type_filter: Option<MemoryType>,
    ) -> Result<Vec<RetrievalResult>> {
        let experiences = self
            .store
            .search_by_keyword(query, tenant_id, limit.max(50), memory_type_filter)
            .await?;
        let query_terms = tokenize(query);
        let mut results = Vec::with_capacity(experiences.len());
        for exp in experiences {
            let keyword_score = bm25_score(&query_terms, &exp.content);
            let importance = exp.confidence;
            let score = keyword_score * WEIGHT_KEYWORD_ONLY + importance * WEIGHT_IMPORTANCE_ONLY;
            results.push(RetrievalResult {
                experience: exp,
                score,
                keyword_score,
                semantic_score: 0.0,
                temporal_score: 0.0,
                external_score: 0.0,
                is_external: false,
            });
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        Ok(results)
    }

    /// Vector-only search: embed query, search by cosine similarity.
    async fn vector_search(
        &self,
        query: &str,
        tenant_id: &str,
        limit: usize,
        memory_type_filter: Option<MemoryType>,
    ) -> Result<Vec<RetrievalResult>> {
        if !self.embedder.enabled() {
            return Err(Error::InvalidInput(
                "vector search requires an enabled embedding provider".into(),
            ));
        }
        let query_vec = self.embedder.embed(query).await?;
        let mut experiences = self
            .store
            .search_by_vector(&query_vec, tenant_id, limit)
            .await?;
        if let Some(mt) = memory_type_filter {
            experiences.retain(|e| e.memory_type == mt);
        }
        let mut results = Vec::with_capacity(experiences.len());
        for exp in experiences {
            // sqlite-vec reports cosine distance in `[0, 2]`; convert to
            // similarity in `[0, 1]` so the weighted-fusion formula stays
            // honest.
            let semantic_score = (1.0 - exp.distance).clamp(0.0, 1.0);
            let importance = exp.confidence;
            let score = semantic_score * WEIGHT_SEMANTIC + importance * WEIGHT_IMPORTANCE_HYBRID;
            results.push(RetrievalResult {
                experience: exp,
                score,
                keyword_score: 0.0,
                semantic_score,
                temporal_score: 0.0,
                external_score: 0.0,
                is_external: false,
            });
        }
        results.truncate(limit);
        Ok(results)
    }

    /// Hybrid search: combine keyword and vector signals with ranking fusion.
    async fn hybrid_search(
        &self,
        query: &str,
        tenant_id: &str,
        limit: usize,
        memory_type_filter: Option<MemoryType>,
    ) -> Result<Vec<RetrievalResult>> {
        let query_vec = if self.embedder.enabled() {
            self.embedder.embed(query).await?
        } else {
            Vec::new()
        };
        let candidates = self
            .store
            .search_by_keyword(query, tenant_id, limit.max(50), memory_type_filter)
            .await?;
        let query_terms = tokenize(query);

        // Build a map of experience id -> cosine similarity (if embeddings
        // available). sqlite-vec reports cosine distance in `[0, 2]`; we
        // convert to similarity `1.0 - distance`, clamped to `[0, 1]`.
        let mut semantic_map: HashMap<String, f64> = HashMap::new();
        if !query_vec.is_empty() {
            let vector_results = self
                .store
                .search_by_vector(&query_vec, tenant_id, candidates.len().max(limit))
                .await?;
            for exp in &vector_results {
                let similarity = (1.0 - exp.distance).clamp(0.0, 1.0);
                semantic_map.insert(exp.id.clone(), similarity);
            }
        }

        let mut results = Vec::with_capacity(candidates.len());
        // Rank each candidate by keyword score and semantic score, then fuse
        // with Reciprocal Rank Fusion (RRF). RRF is scale-free: unlike the
        // old linear weighted sum, it does not let the larger-score signal
        // (BM25 vs cosine) dominate simply because its scale is bigger.
        let mut keyword_ranked: Vec<(&Experience, f64)> = candidates
            .iter()
            .map(|exp| {
                let kw = bm25_score(&query_terms, &exp.content);
                (exp, kw)
            })
            .collect();
        keyword_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Semantic ranking contains ONLY documents with a real vector hit
        // (present in `semantic_map`). Documents absent from it are keyword-only
        // and receive a single RRF contribution; including them here would give
        // them a full second term based on arbitrary zero-score order.
        let mut semantic_ranked: Vec<(&Experience, f64)> = candidates
            .iter()
            .filter(|exp| semantic_map.contains_key(&exp.id))
            .map(|exp| (exp, semantic_map.get(&exp.id).copied().unwrap_or(0.0)))
            .collect();
        semantic_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // O(1) rank lookup instead of a per-candidate O(n) position scan.
        let semantic_rank_of: HashMap<&str, usize> = semantic_ranked
            .iter()
            .enumerate()
            .map(|(rank, (exp, _))| (exp.id.as_str(), rank))
            .collect();

        // Temporal relevance (mem0-style time-aware retrieval): detect the
        // query intent once and score every candidate by its timestamps.
        // Unlike keyword/semantic (rank-based RRF), time is fused as a
        // weighted additive term — rank-only fusion cancels out when keyword
        // and temporal orders are reversed across exactly two candidates
        // (1/(K+0)+1/(K+1) is symmetric), so a score-bounded additive term is
        // the decisive-yet-bounded way to break ties. Current/Future queries
        // prefer fresh, still-valid instances; Past queries prefer older ones.
        let intent = TimeIntent::from_query(query);
        let now = Utc::now();
        let temporal_of: HashMap<&str, f64> = candidates
            .iter()
            .map(|exp| {
                let t = temporal_score(intent, exp.created_at, exp.expires_at, now);
                (exp.id.as_str(), t)
            })
            .collect();

        // RRF constant; 60 is the standard value used by Elasticsearch.
        const RRF_K: f64 = 60.0;

        for (i, (exp, kw)) in keyword_ranked.iter().enumerate() {
            // Documents in BOTH lists get two rank contributions; keyword-only
            // documents get a single term (`semantic_rank` stays MAX → term≈0).
            let semantic_rank = semantic_rank_of
                .get(exp.id.as_str())
                .copied()
                .unwrap_or(usize::MAX);
            let rrf = 1.0 / (RRF_K + i as f64) + 1.0 / (RRF_K + semantic_rank as f64);
            // Importance remains a small, scale-bounded tiebreaker.
            let importance = exp.confidence * WEIGHT_IMPORTANCE_HYBRID;
            // Temporal relevance is a second scale-bounded additive term,
            // scaled by the query intent's per-instance score.
            let temporal =
                temporal_of.get(exp.id.as_str()).copied().unwrap_or(0.0) * WEIGHT_TEMPORAL_HYBRID;
            let score = rrf + importance + temporal;
            if score <= 0.0 {
                continue;
            }
            results.push(RetrievalResult {
                experience: (*exp).clone(),
                score,
                keyword_score: *kw,
                semantic_score: semantic_map.get(&exp.id).copied().unwrap_or(0.0),
                temporal_score: temporal_of.get(exp.id.as_str()).copied().unwrap_or(0.0),
                external_score: 0.0,
                is_external: false,
            });
        }

        // External-signal fusion (external-knowledge-plan §B4). When a registry
        // is attached, forward the query to every signal provider and merge the
        // hits as a THIRD RRF list. Each external hit becomes a synthetic
        // candidate: it contributes only its external RRF term (no local
        // keyword/semantic signal, since external ids are opaque to Mnemosyne
        // and cannot be reliably matched to local experiences). This keeps the
        // fusion scale-free and never double-counts a hit across lists.
        if let Some(registry) = &self.external {
            let external_hits = registry.search_all(query, limit);
            for (rank, hit) in external_hits.iter().enumerate() {
                let external_rrf = 1.0 / (RRF_K + rank as f64);
                // External score carries the source's own relevance as a small
                // importance tiebreaker, mirroring the local importance weight.
                let importance = hit.score * WEIGHT_IMPORTANCE_HYBRID;
                let score = external_rrf + importance;
                // Build a synthetic, non-persisted Experience so the existing
                // RetrievalResult shape is preserved. The id is namespaced so
                // callers can distinguish `ext:` hits from real local ids and
                // never accidentally overwrite a stored experience.
                let mut exp = Experience::new(
                    tenant_id,
                    MemoryType::Knowledge,
                    hit.text.clone(),
                    hit.score,
                );
                exp.id = format!("ext:{}:{}", hit.source, hit.id);
                exp.source = hit.source.clone();
                results.push(RetrievalResult {
                    experience: exp,
                    score,
                    keyword_score: 0.0,
                    semantic_score: 0.0,
                    temporal_score: 0.0,
                    external_score: external_rrf,
                    is_external: true,
                });
            }
        }

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        Ok(results)
    }
}

pub(crate) fn tokenize(text: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "a", "an", "the", "and", "or", "but", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could", "should", "may",
        "might", "must", "shall", "can", "need", "dare", "ought", "used", "to", "of", "in", "for",
        "on", "with", "at", "by", "from", "up", "about", "into", "through", "during", "before",
        "after", "above", "below", "between", "under", "i", "you", "he", "she", "it", "we", "they",
        "me", "him", "her", "us", "them", "my", "your", "his", "its", "our", "their",
    ];
    text.split(|c: char| !c.is_alphanumeric())
        .filter_map(|s| {
            let lower = s.to_lowercase();
            if lower.is_empty() || lower.len() == 1 || STOPWORDS.contains(&lower.as_str()) {
                None
            } else {
                Some(lower)
            }
        })
        .collect()
}

/// Compute a BM25-style score for a query against a document.
///
/// This is a simplified BM25 variant: for each query term present in the
/// document, accumulate `term_freq / (term_freq + k1)`. The score is then
/// normalized to `[0.0, 1.0]` by dividing by the number of query terms.
///
/// # Arguments
///
/// * `query_terms` - Pre-tokenized query terms (lowercase).
/// * `document` - The document text to score against.
pub(crate) fn bm25_score(query_terms: &[String], document: &str) -> f64 {
    if query_terms.is_empty() {
        return 0.0;
    }
    const K1: f64 = 1.2;
    let doc_lower = document.to_lowercase();
    let doc_terms: Vec<&str> = doc_lower.split(|c: char| !c.is_alphanumeric()).collect();
    let mut score = 0.0_f64;
    for qterm in query_terms {
        let term_freq = doc_terms.iter().filter(|t| **t == qterm.as_str()).count();
        if term_freq > 0 {
            let tf = term_freq as f64;
            score += tf / (tf + K1);
        }
    }
    // Normalize by number of query terms to keep score in [0, 1].
    score / query_terms.len() as f64
}
