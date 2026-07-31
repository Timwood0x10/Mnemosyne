//! Hybrid retrieval layer.
//!
//! Per `improve.md` Section 7, retrieval is *Hybrid Retrieval* — a fusion of
//! keyword (BM25/FTS) and vector (sqlite-vec cosine similarity) signals, ranked
//! by a configurable scoring formula.
//!
//! ## Modes
//!
//! - [`RetrievalMode::Keyword`]: BM25 over memory content. Default when no
//!   embedding provider is configured (`embedding_provider = none`).
//! - [`RetrievalMode::Vector`]: cosine similarity via the store's vector
//!   index. Requires an active embedding provider.
//! - [`RetrievalMode::Hybrid`]: keyword + vector + ranking fusion.
//!
//! ## Ranking formula
//!
//! When embeddings are present:
//!
//! ```text
//! score = semantic_score * 0.6 + keyword_score * 0.2 + importance * 0.2
//! ```
//!
//! When no embedding is available (keyword-only mode):
//!
//! ```text
//! score = keyword_score * 0.7 + importance * 0.3
//! ```
//!
//! The module is storage-agnostic: it delegates vector search to
//! [`crate::store::ExperienceRepository`] and performs keyword scoring
//! in-memory over the candidate set.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::config::{
    RetrievalMode, WEIGHT_IMPORTANCE_HYBRID, WEIGHT_IMPORTANCE_ONLY, WEIGHT_KEYWORD_ONLY,
    WEIGHT_SEMANTIC,
};
use crate::embed::EmbeddingService;
use crate::error::{Error, Result};
use crate::knowledge::ExternalKnowledgeRegistry;
use crate::store::ExperienceRepository;
use crate::types::{Experience, MemoryType};

/// A single retrieval result with its computed score.
#[derive(Debug, Clone, Serialize)]
pub struct RetrievalResult {
    /// The matched experience record.
    pub experience: Experience,
    /// Final fused score in `[0.0, 1.0]` (higher is better).
    pub score: f64,
    /// Keyword (BM25-style) component score.
    pub keyword_score: f64,
    /// Vector similarity component score (0.0 when no embedding).
    pub semantic_score: f64,
    /// External-signal RRF contribution (0.0 for local-only results).
    ///
    /// Populated only for synthetic candidates produced by an external
    /// [`ExternalKnowledgeRegistry`] signal provider. Local results that did
    /// not come from an external source keep this at 0.0.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub external_score: f64,
    /// `true` when this result is a synthetic external hit (not a persisted
    /// experience). Consumers can filter these out when only local memories
    /// are admissible.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_external: bool,
}

/// `skip_serializing_if` helper: true when the score is exactly `0.0`.
fn is_zero_f64(v: &f64) -> bool {
    v.abs() < f64::EPSILON
}

/// The retrieval engine.
///
/// Holds references to the embedding service (for query embedding in
/// vector/hybrid modes) and the experience repository (for vector search
/// and candidate fetching).
pub struct RetrievalEngine {
    embedder: Arc<dyn EmbeddingService>,
    store: Arc<dyn ExperienceRepository>,
    mode: RetrievalMode,
    /// Optional external-knowledge registry. When attached, hybrid search fuses
    /// each signal provider's hits into the RRF ranking as synthetic candidates
    /// (external-knowledge-plan §B4). `None` keeps retrieval purely local.
    external: Option<Arc<ExternalKnowledgeRegistry>>,
}

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
            let score = rrf + importance;
            if score <= 0.0 {
                continue;
            }
            results.push(RetrievalResult {
                experience: (*exp).clone(),
                score,
                keyword_score: *kw,
                semantic_score: semantic_map.get(&exp.id).copied().unwrap_or(0.0),
                external_score: 0.0,
                is_external: false,
            });
        }

        // External-signal fusion (external-knowledge-plan §B4). When a registry
        // is attached, forward the query to every signal provider and merge the
        // hits as a THIRD RRF list. Each external hit becomes a synthetic
        // candidate: it contributes only its external RRF term (no local
        // keyword/semantic signal, since external ids are opaque to LoreScope
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::NullEmbedder;
    use crate::store::SQLiteVecStore;
    use crate::types::Experience;

    /// Objective: Verify tokenize splits, lowercases, and removes stopwords.
    /// Invariants: "The Rust Borrow Checker" -> ["rust", "borrow", "checker"].
    #[test]
    fn tokenize_removes_stopwords_and_lowercases() {
        let tokens = tokenize("The Rust Borrow Checker");
        assert_eq!(
            tokens,
            vec!["rust", "borrow", "checker"],
            "stopwords removed, lowercased"
        );
    }

    /// Objective: Verify tokenize handles empty and single-char input.
    /// Invariants: Empty input -> empty; single chars -> empty.
    #[test]
    fn tokenize_handles_empty_and_single_chars() {
        assert!(tokenize("").is_empty(), "empty input");
        assert!(tokenize("a b c").is_empty(), "single chars filtered");
    }

    /// Objective: Verify bm25_score returns 0 for empty query terms.
    /// Invariants: No query terms -> score 0.
    #[test]
    fn bm25_score_empty_query_returns_zero() {
        let score = bm25_score(&[], "any document");
        assert!(
            score.abs() < f64::EPSILON,
            "empty query must score 0, got {score}"
        );
    }

    /// Objective: Verify bm25_score returns 0 when no terms match.
    /// Invariants: Disjoint terms -> score 0.
    #[test]
    fn bm25_score_no_match_returns_zero() {
        let query = vec!["rust".to_string(), "async".to_string()];
        let score = bm25_score(&query, "python concurrency model");
        assert!(
            score.abs() < f64::EPSILON,
            "no matching terms must score 0, got {score}"
        );
    }

    /// Objective: Verify bm25_score is positive when terms match.
    /// Invariants: At least one match -> score > 0.
    #[test]
    fn bm25_score_match_returns_positive() {
        let query = vec!["rust".to_string(), "async".to_string()];
        let score = bm25_score(&query, "rust async runtime uses tokio");
        assert!(score > 0.0, "matching terms must score > 0, got {score}");
        assert!(score <= 1.0, "normalized score must be <= 1.0, got {score}");
    }

    /// Objective: Verify bm25_score rewards higher term frequency.
    /// Invariants: More occurrences of a term -> higher score.
    #[test]
    fn bm25_score_rewards_term_frequency() {
        let query = vec!["rust".to_string()];
        let low_freq = bm25_score(&query, "rust rust");
        let high_freq = bm25_score(&query, "rust rust rust rust");
        assert!(
            high_freq > low_freq,
            "higher term frequency must score higher: {high_freq} vs {low_freq}"
        );
    }

    /// Objective: Verify keyword search returns ranked results.
    /// Invariants: Search for "rust async" returns matching memories, ranked.
    #[tokio::test]
    async fn keyword_search_ranks_results() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        let engine = RetrievalEngine::new(embedder, store.clone(), RetrievalMode::Keyword);

        let mut exp1 = Experience::new(
            "t1",
            MemoryType::Knowledge,
            "rust async runtime uses tokio",
            0.8,
        );
        exp1.id = "e1".to_string();
        let mut exp2 = Experience::new(
            "t1",
            MemoryType::Knowledge,
            "python gil concurrency model",
            0.7,
        );
        exp2.id = "e2".to_string();
        let mut exp3 = Experience::new(
            "t1",
            MemoryType::Knowledge,
            "rust borrow checker prevents data races",
            0.9,
        );
        exp3.id = "e3".to_string();

        store.create(&exp1).await.expect("create e1");
        store.create(&exp2).await.expect("create e2");
        store.create(&exp3).await.expect("create e3");

        let results = engine
            .search("rust async", "t1", 5, None)
            .await
            .expect("search");
        assert!(!results.is_empty(), "must return results");
        // e1 (rust async) should rank highest.
        assert_eq!(
            results[0].experience.id,
            "e1",
            "top result must be e1 (matches both terms), got {:?}",
            results.iter().map(|r| &r.experience.id).collect::<Vec<_>>()
        );
    }

    /// Objective: Verify keyword search respects tenant isolation.
    /// Invariants: Search in t2 returns nothing for t1 memories.
    #[tokio::test]
    async fn keyword_search_respects_tenant_isolation() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        let engine = RetrievalEngine::new(embedder, store.clone(), RetrievalMode::Keyword);

        let mut exp = Experience::new("t1", MemoryType::Knowledge, "rust async runtime", 0.8);
        exp.id = "e1".to_string();
        store.create(&exp).await.expect("create");

        let results = engine.search("rust", "t2", 5, None).await.expect("search");
        assert!(
            results.is_empty(),
            "tenant t2 search must return nothing for t1 memories"
        );
    }

    /// Objective: Verify keyword search respects memory_type filter.
    /// Invariants: Filter by Preference returns only Preference memories.
    #[tokio::test]
    async fn keyword_search_respects_memory_type_filter() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        let engine = RetrievalEngine::new(embedder, store.clone(), RetrievalMode::Keyword);

        let mut k_exp = Experience::new("t1", MemoryType::Knowledge, "rust async runtime", 0.8);
        k_exp.id = "k1".to_string();
        let mut p_exp = Experience::new(
            "t1",
            MemoryType::Preference,
            "prefer rust async runtime",
            0.7,
        );
        p_exp.id = "p1".to_string();
        store.create(&k_exp).await.expect("create k");
        store.create(&p_exp).await.expect("create p");

        let results = engine
            .search("rust", "t1", 5, Some(MemoryType::Preference))
            .await
            .expect("search");
        assert!(
            results
                .iter()
                .all(|r| r.experience.memory_type == MemoryType::Preference),
            "filter must return only Preference memories"
        );
    }

    /// Objective: Verify keyword search with limit=0 returns empty.
    /// Invariants: limit=0 -> empty results, no error.
    #[tokio::test]
    async fn keyword_search_limit_zero_returns_empty() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        let engine = RetrievalEngine::new(embedder, store, RetrievalMode::Keyword);

        let results = engine.search("rust", "t1", 0, None).await.expect("search");
        assert!(results.is_empty(), "limit=0 must return empty results");
    }

    /// Objective: Verify RetrievalResult score fields are populated.
    /// Invariants: keyword_score > 0 for matching results; semantic_score = 0 in keyword mode.
    #[tokio::test]
    async fn retrieval_result_score_fields_populated() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        let engine = RetrievalEngine::new(embedder, store.clone(), RetrievalMode::Keyword);

        let mut exp = Experience::new("t1", MemoryType::Knowledge, "rust async runtime", 0.8);
        exp.id = "e1".to_string();
        store.create(&exp).await.expect("create");

        let results = engine.search("rust", "t1", 5, None).await.expect("search");
        assert!(!results.is_empty(), "must return results");
        let r = &results[0];
        assert!(r.keyword_score > 0.0, "keyword_score must be > 0 for match");
        assert!(
            r.semantic_score.abs() < f64::EPSILON,
            "semantic_score must be 0 in keyword mode, got {}",
            r.semantic_score
        );
        assert!(r.score > 0.0, "final score must be > 0");
    }

    /// Objective: Verify bm25_score normalizes to [0, 1].
    /// Invariants: Even with many matches, score <= 1.0.
    #[test]
    fn bm25_score_normalized_to_unit_interval() {
        let query = vec![
            "rust".to_string(),
            "async".to_string(),
            "runtime".to_string(),
        ];
        // Document matches all query terms multiple times.
        let score = bm25_score(&query, "rust rust async async runtime runtime");
        assert!(score <= 1.0, "normalized score must be <= 1.0, got {score}");
        assert!(score > 0.0, "positive score for matches");
    }

    /// Objective: Verify RRF fusion rewards documents found by BOTH signals.
    /// Invariants: A doc that ranks #1 in keyword AND #1 in semantic gets a
    /// higher fused score than a doc ranked #1 in keyword but absent from the
    /// semantic list — the scale-free RRF must not be dominated by one signal.
    #[test]
    fn rrf_prefers_docs_found_by_both_signals() {
        // Doc A: strong keyword match, weak semantic (low rank).
        // Doc B: moderate keyword match, absent from semantic (high rank / MAX).
        let mut doc_a = Experience::new("t1", MemoryType::Knowledge, "rust rust rust async", 0.5);
        doc_a.id = "a".to_string();
        let mut doc_b = Experience::new("t1", MemoryType::Knowledge, "rust async", 0.5);
        doc_b.id = "b".to_string();
        let candidates = [doc_a, doc_b];

        let query_terms = tokenize("rust async");
        // Fake semantic map: A present (similarity 0.3), B absent.
        let mut semantic_map: HashMap<String, f64> = HashMap::new();
        semantic_map.insert("a".to_string(), 0.3);

        // Replicate the hybrid ranking logic: rank by keyword, then fuse.
        let mut keyword_ranked: Vec<(&Experience, f64)> = candidates
            .iter()
            .map(|exp| (exp, bm25_score(&query_terms, &exp.content)))
            .collect();
        keyword_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut semantic_ranked: Vec<(&Experience, f64)> = candidates
            .iter()
            .map(|exp| (exp, semantic_map.get(&exp.id).copied().unwrap_or(0.0)))
            .collect();
        semantic_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        const RRF_K: f64 = 60.0;
        let fused = |rank: usize| 1.0 / (RRF_K + rank as f64);

        let a_rank = keyword_ranked
            .iter()
            .position(|(e, _)| e.id == "a")
            .unwrap_or(usize::MAX);
        let a_sem_rank = semantic_ranked
            .iter()
            .position(|(e, _)| e.id == "a")
            .unwrap_or(usize::MAX);
        let b_rank = keyword_ranked
            .iter()
            .position(|(e, _)| e.id == "b")
            .unwrap_or(usize::MAX);
        let b_sem_rank = semantic_ranked
            .iter()
            .position(|(e, _)| e.id == "b")
            .unwrap_or(usize::MAX);

        let a_score = fused(a_rank) + fused(a_sem_rank);
        let b_score = fused(b_rank) + fused(b_sem_rank);

        assert!(
            a_score > b_score,
            "doc present in both keyword+semantic must fuse higher than doc in only one; a={a_score}, b={b_score}"
        );
    }

    // ── External-signal fusion (external-knowledge-plan §B4) ────────────────

    /// Build a registry carrying a single Db adapter that returns `hits` for
    /// any query, wrapped in an `Arc` ready to attach to a RetrievalEngine.
    fn registry_with_db_hits(
        hits: Vec<crate::knowledge::adapter::ExternalHit>,
    ) -> Arc<ExternalKnowledgeRegistry> {
        let reg = ExternalKnowledgeRegistry::new();
        let captured = hits;
        let adapter = crate::knowledge::adapter::DbAdapter::new(
            "fake-db",
            crate::knowledge::adapter::SchemaMapping::default(),
            Box::new(move |_, _| captured.clone()),
        );
        reg.register_signal(adapter);
        Arc::new(reg)
    }

    /// Objective: Verify hybrid search with an external registry surfaces
    /// synthetic external hits alongside local results, each marked
    /// `is_external=true` with a namespaced `ext:` id.
    /// Invariants: At least one external result appears; its id starts with
    /// `ext:`; is_external is true; external_score > 0.
    #[tokio::test]
    async fn hybrid_search_fuses_external_hits() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        let registry = registry_with_db_hits(vec![crate::knowledge::adapter::ExternalHit {
            id: "row-9".into(),
            text: "external knowledge from db".into(),
            score: 0.8,
            source: "fake-db".into(),
        }]);
        let engine = RetrievalEngine::new(embedder, store, RetrievalMode::Hybrid)
            .with_external_registry(registry);

        let results = engine
            .search("anything", "t1", 5, None)
            .await
            .expect("search");
        let external: Vec<_> = results.iter().filter(|r| r.is_external).collect();
        assert!(
            !external.is_empty(),
            "at least one external hit must surface, got: {:?}",
            results.iter().map(|r| &r.experience.id).collect::<Vec<_>>()
        );
        let r = external[0];
        assert!(
            r.experience.id.starts_with("ext:fake-db:"),
            "external id must be namespaced, got {}",
            r.experience.id
        );
        assert!(r.external_score > 0.0, "external_score must be populated");
        assert!(
            r.semantic_score.abs() < f64::EPSILON,
            "external hits carry no semantic score"
        );
        assert_eq!(
            r.experience.source, "fake-db",
            "external hit records its source for provenance"
        );
    }

    /// Objective: Verify a high-scoring external hit can OUTRANK a weak local
    /// keyword-only hit, so external knowledge is genuinely competitive in the
    /// fused ranking — not just appended at the bottom.
    /// Invariants: Top external hit (rank 0, score 1.0) beats a low-confidence
    /// local hit that only weakly matches the query.
    #[tokio::test]
    async fn hybrid_external_hit_can_outrank_weak_local() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        // Local memory: weak keyword match (single term, low confidence).
        let mut local = Experience::new("t1", MemoryType::Knowledge, "rust", 0.1);
        local.id = "local-1".to_string();
        store.create(&local).await.expect("create");

        let registry = registry_with_db_hits(vec![crate::knowledge::adapter::ExternalHit {
            id: "strong".into(),
            text: "comprehensive rust async guide from external db".into(),
            score: 1.0,
            source: "fake-db".into(),
        }]);
        let engine = RetrievalEngine::new(embedder, store, RetrievalMode::Hybrid)
            .with_external_registry(registry);

        let results = engine.search("rust", "t1", 5, None).await.expect("search");
        // The external hit (rank 0 → rrf 1/60 ≈ 0.0167 + 0.2 importance) must
        // beat the local hit (keyword rrf 1/60 + 0.02 importance). Both have the
        // same keyword RRF term, but the external importance (1.0 * 0.2 = 0.2)
        // dwarfs the local importance (0.1 * 0.2 = 0.02), so external wins.
        assert!(!results.is_empty(), "must return results");
        assert!(
            results[0].is_external,
            "high-score external hit must rank first, got id={}",
            results[0].experience.id
        );
    }

    /// Objective: Verify hybrid search WITHOUT an external registry behaves
    /// exactly as before (no synthetic candidates, all is_external=false).
    /// Invariants: Every result is local (is_external false, external_score 0).
    #[tokio::test]
    async fn hybrid_without_registry_has_no_external_results() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        let mut exp = Experience::new("t1", MemoryType::Knowledge, "rust async", 0.8);
        exp.id = "e1".to_string();
        store.create(&exp).await.expect("create");
        // No with_external_registry call → external stays None.
        let engine = RetrievalEngine::new(embedder, store, RetrievalMode::Hybrid);

        let results = engine.search("rust", "t1", 5, None).await.expect("search");
        assert!(
            results.iter().all(|r| !r.is_external),
            "no external results without a registry"
        );
        assert!(
            results
                .iter()
                .all(|r| r.external_score.abs() < f64::EPSILON),
            "external_score stays 0 without a registry"
        );
    }

    /// Objective: Verify keyword/vector modes are UNAFFECTED by an attached
    /// external registry (the plan scopes external fusion to hybrid only).
    /// Invariants: Keyword search returns no external hits even when a registry
    /// is attached.
    #[tokio::test]
    async fn keyword_mode_ignores_external_registry() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        let mut exp = Experience::new("t1", MemoryType::Knowledge, "rust async", 0.8);
        exp.id = "e1".to_string();
        store.create(&exp).await.expect("create");
        let registry = registry_with_db_hits(vec![crate::knowledge::adapter::ExternalHit {
            id: "ext".into(),
            text: "should not appear".into(),
            score: 1.0,
            source: "db".into(),
        }]);
        let engine = RetrievalEngine::new(embedder, store, RetrievalMode::Keyword)
            .with_external_registry(registry);

        let results = engine.search("rust", "t1", 5, None).await.expect("search");
        assert!(
            results.iter().all(|r| !r.is_external),
            "keyword mode must not fuse external signals"
        );
    }

    /// Objective: Verify set_external_registry (runtime attach) makes external
    /// hits visible on the next search — mirrors the knowledge_attach MCP flow.
    /// Invariants: Before attach, no external hits; after attach, external hits
    /// appear.
    #[tokio::test]
    async fn set_external_registry_attaches_at_runtime() {
        let store = Arc::new(SQLiteVecStore::open_in_memory(4).await.expect("open"));
        let embedder: Arc<dyn EmbeddingService> = Arc::new(NullEmbedder::new());
        let mut engine = RetrievalEngine::new(embedder, store, RetrievalMode::Hybrid);
        // No registry yet → no external results.
        let before = engine.search("q", "t1", 5, None).await.expect("search");
        assert!(before.iter().all(|r| !r.is_external));

        engine.set_external_registry(registry_with_db_hits(vec![
            crate::knowledge::adapter::ExternalHit {
                id: "r1".into(),
                text: "late-attached".into(),
                score: 0.9,
                source: "db".into(),
            },
        ]));
        let after = engine.search("q", "t1", 5, None).await.expect("search");
        assert!(
            after.iter().any(|r| r.is_external),
            "after set_external_registry, external hits must appear"
        );
    }
}
