//! Embedding cache — avoids redundant embedding calls for repeated mentions.
//!
//! In a typical novel, the same mention (e.g. "刘备") appears thousands of
//! times. The cache stores the result of the first embedding call and returns
//! it for subsequent mentions, reducing embedding calls by ≈100×.

use std::collections::HashMap;

/// Cache for mention → vector lookups.
pub trait EmbeddingCache: Send + Sync {
    /// Retrieve a cached vector for `mention`, if present.
    fn get(&self, mention: &str) -> Option<Vec<f32>>;

    /// Store a vector for `mention`.
    fn put(&mut self, mention: &str, vec: Vec<f32>);

    /// Number of entries in the cache.
    fn len(&self) -> usize;

    /// Returns `true` if the cache is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// In-memory HashMap-based embedding cache. V1 default.
pub struct MemoryEmbeddingCache {
    cache: HashMap<String, Vec<f32>>,
}

impl MemoryEmbeddingCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        MemoryEmbeddingCache {
            cache: HashMap::new(),
        }
    }
}

impl Default for MemoryEmbeddingCache {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingCache for MemoryEmbeddingCache {
    fn get(&self, mention: &str) -> Option<Vec<f32>> {
        self.cache.get(mention).cloned()
    }

    fn put(&mut self, mention: &str, vec: Vec<f32>) {
        self.cache.insert(mention.to_string(), vec);
    }

    fn len(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that a stored vector is returned by `get`.
    /// Invariants: The cached vector matches the original.
    #[test]
    fn get_returns_stored_vector() {
        let mut cache = MemoryEmbeddingCache::new();
        let vec = vec![0.1, 0.2, 0.3];
        cache.put("玄德", vec.clone());
        let got = cache.get("玄德");
        assert_eq!(got, Some(vec), "stored vector should be returned");
    }

    /// Objective: Verify that an unknown mention returns None.
    /// Invariants: No false cache hits.
    #[test]
    fn unknown_mention_returns_none() {
        let cache = MemoryEmbeddingCache::new();
        assert_eq!(
            cache.get("unknown"),
            None,
            "unknown mention should return None"
        );
    }

    /// Objective: Verify that `len` reflects the actual number of entries.
    /// Invariants: After 3 insertions, len is 3.
    #[test]
    fn len_tracks_insertions() {
        let mut cache = MemoryEmbeddingCache::new();
        assert!(cache.is_empty());
        cache.put("a", vec![1.0]);
        cache.put("b", vec![2.0]);
        cache.put("c", vec![3.0]);
        assert_eq!(cache.len(), 3, "cache should track 3 entries");
    }

    /// Objective: Verify that overriding an existing key updates its vector.
    /// Invariants: The new value replaces the old one.
    #[test]
    fn put_overwrites_existing_key() {
        let mut cache = MemoryEmbeddingCache::new();
        cache.put("玄德", vec![0.1, 0.2]);
        cache.put("玄德", vec![0.3, 0.4]);
        let got = cache.get("玄德");
        assert_eq!(got, Some(vec![0.3, 0.4]), "latest put should overwrite");
    }
}
