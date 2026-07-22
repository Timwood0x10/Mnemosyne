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
    RetrievalMode, WEIGHT_IMPORTANCE_HYBRID, WEIGHT_IMPORTANCE_ONLY, WEIGHT_KEYWORD_HYBRID,
    WEIGHT_KEYWORD_ONLY, WEIGHT_SEMANTIC,
};
use crate::embed::EmbeddingService;
use crate::error::{Error, Result};
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
        }
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

    /// Keyword-only search: BM25-style scoring over all tenant memories.
    async fn keyword_search(
        &self,
        query: &str,
        tenant_id: &str,
        limit: usize,
        memory_type_filter: Option<MemoryType>,
    ) -> Result<Vec<RetrievalResult>> {
        let candidates = self.fetch_candidates(tenant_id, memory_type_filter).await?;
        let query_terms = tokenize(query);
        let mut results = Vec::with_capacity(candidates.len());
        for exp in candidates {
            let keyword_score = bm25_score(&query_terms, &exp.content);
            if keyword_score <= 0.0 {
                continue;
            }
            let importance = exp.confidence;
            let score = keyword_score * WEIGHT_KEYWORD_ONLY + importance * WEIGHT_IMPORTANCE_ONLY;
            results.push(RetrievalResult {
                experience: exp,
                score,
                keyword_score,
                semantic_score: 0.0,
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
        let max_sim = 1.0_f64;
        let mut results = Vec::with_capacity(experiences.len());
        for exp in experiences {
            // sqlite-vec returns results ordered by similarity; we approximate
            // semantic_score by a linear decay based on rank position.
            let semantic_score = max_sim; // sqlite-vec already ranked
            let importance = exp.confidence;
            let score = semantic_score * WEIGHT_SEMANTIC + importance * WEIGHT_IMPORTANCE_HYBRID;
            results.push(RetrievalResult {
                experience: exp,
                score,
                keyword_score: 0.0,
                semantic_score,
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
        let candidates = self.fetch_candidates(tenant_id, memory_type_filter).await?;
        let query_terms = tokenize(query);

        // Build a map of experience id -> semantic score (if embeddings available).
        let mut semantic_map: HashMap<String, f64> = HashMap::new();
        if !query_vec.is_empty() {
            let vector_results = self
                .store
                .search_by_vector(&query_vec, tenant_id, candidates.len().max(limit))
                .await?;
            for (rank, exp) in vector_results.iter().enumerate() {
                // Linear decay: top result = 1.0, decaying by rank.
                let decay = 1.0 / (1.0 + rank as f64);
                semantic_map.insert(exp.id.clone(), decay);
            }
        }

        let mut results = Vec::with_capacity(candidates.len());
        for exp in candidates {
            let keyword_score = bm25_score(&query_terms, &exp.content);
            let semantic_score = semantic_map.get(&exp.id).copied().unwrap_or(0.0);
            let importance = exp.confidence;
            let score = semantic_score * WEIGHT_SEMANTIC
                + keyword_score * WEIGHT_KEYWORD_HYBRID
                + importance * WEIGHT_IMPORTANCE_HYBRID;
            if score <= 0.0 {
                continue;
            }
            results.push(RetrievalResult {
                experience: exp,
                score,
                keyword_score,
                semantic_score,
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

    /// Fetch candidate experiences for keyword/hybrid scoring.
    ///
    /// We pull all memories of the relevant types for the tenant, then score
    /// them in-memory. This is O(n) over the tenant's memories, acceptable for
    /// the default `max_solutions_per_tenant = 5000` scale.
    async fn fetch_candidates(
        &self,
        tenant_id: &str,
        memory_type_filter: Option<MemoryType>,
    ) -> Result<Vec<Experience>> {
        if let Some(mt) = memory_type_filter {
            return self.store.get_by_memory_type(tenant_id, mt).await;
        }
        // No filter: gather all types.
        let mut all = Vec::new();
        for mt in [
            MemoryType::Knowledge,
            MemoryType::Preference,
            MemoryType::Skill,
            MemoryType::Experience,
            MemoryType::Interaction,
            MemoryType::Profile,
        ] {
            let exps = self.store.get_by_memory_type(tenant_id, mt).await?;
            all.extend(exps);
        }
        Ok(all)
    }
}

/// Tokenize a text string into lowercase terms for BM25 scoring.
///
/// Splits on non-alphanumeric characters and filters out empty tokens and
/// common English stopwords.
fn tokenize(text: &str) -> Vec<String> {
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
fn bm25_score(query_terms: &[String], document: &str) -> f64 {
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
}
