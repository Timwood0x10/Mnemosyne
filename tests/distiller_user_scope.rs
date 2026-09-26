//! Integration: distillation conflict resolution is scoped to ONE user —
//! the vector search is tenant-wide, but neither `ReplaceOld` nor the
//! keyword content-hash dedup may touch another user's memory (T15).
//! Kept out of `distiller/mod.rs` so that file stays under the
//! one-file-per-1000-lines rule (`plan/rules/rules.md` §1).

use std::sync::Arc;

use async_trait::async_trait;
use mnemosyne::distiller::{DistillationConfig, Distiller, PipelineDistiller};
use mnemosyne::embed::EmbeddingService;
use mnemosyne::error::Result;
use mnemosyne::store::{ExperienceRepository, SQLiteVecStore};
use mnemosyne::types::{Experience, MemoryType, Message};

/// Deterministic 8-dim stub embedder (same shape as the in-crate one).
struct StubEmbedder;

#[async_trait]
impl EmbeddingService for StubEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut v: Vec<f32> = text
            .chars()
            .take(8)
            .map(|c| (c as u32 % 256) as f32)
            .collect();
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

/// Embedder that returns NO vector — forces the keyword-fallback branch of
/// conflict resolution (dedup by content hash).
struct EmptyEmbedder;

#[async_trait]
impl EmbeddingService for EmptyEmbedder {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(Vec::new())
    }
    async fn embed_with_prefix(&self, _text: &str, _prefix: &str) -> Result<Vec<f32>> {
        Ok(Vec::new())
    }
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
    fn model(&self) -> &str {
        "empty"
    }
    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
}

/// Objective: Verify conflict resolution never destroys ANOTHER user's
/// memory — the vector search is tenant-wide, but `ReplaceOld` must only
/// fire inside the same `user_id` (T15 cross-user conflict bug).
/// Invariants: alice's identical memory survives bob's distillation
/// (count == 2); no replacement is recorded.
#[tokio::test]
async fn distill_never_replaces_other_users_memory() {
    let store = SQLiteVecStore::open_in_memory(8).await.expect("open");
    let store: Arc<dyn ExperienceRepository> = Arc::new(store);
    let embedder: Arc<dyn EmbeddingService> = Arc::new(StubEmbedder);
    let distiller = PipelineDistiller::new(
        DistillationConfig::default(),
        embedder.clone(),
        store.clone(),
    );

    let problem = "How do I fix the connection refused error in the Rust server?";
    let solution = "Start the server before connecting, or bind to the correct port.";
    let content = format!("Problem: {problem} → Solution: {solution}");

    // Alice's existing memory — identical content + embedding.
    let mut existing = Experience::new("t1", MemoryType::Knowledge, &content, 0.1);
    existing.user_id = "alice".into();
    existing.vector = embedder.embed(&content).await.expect("embed");
    store.create(&existing).await.expect("seed");

    let messages = vec![
        Message::new("user", problem),
        Message::new("assistant", solution),
    ];
    let _out = distiller
        .distill("c2", &messages, "t1", "bob")
        .await
        .expect("distill");

    let count = store.count_for_tenant("t1").await.expect("count");
    assert_eq!(
        count, 2,
        "bob's distillation must not delete alice's identical memory"
    );
    assert_eq!(
        distiller.metrics_ref().snapshot().memories_replaced,
        0,
        "no cross-user replacement may be recorded"
    );
    let left = store
        .get_by_memory_type("t1", MemoryType::Knowledge)
        .await
        .expect("get");
    assert!(
        left.iter().any(|e| e.user_id == "alice"),
        "alice's row must still exist"
    );
}

/// Objective: Verify the KEYWORD fallback path (no vectors) is also
/// same-user scoped — an identical content hash from another user must
/// neither delete that row nor drop the new candidate (T15).
/// Invariants: after bob distills alice's exact content with an empty
/// embedder, both rows exist (count == 2).
#[tokio::test]
async fn keyword_dedup_is_scoped_to_same_user() {
    let store = SQLiteVecStore::open_in_memory(8).await.expect("open");
    let store: Arc<dyn ExperienceRepository> = Arc::new(store);
    let embedder: Arc<dyn EmbeddingService> = Arc::new(EmptyEmbedder);
    let distiller = PipelineDistiller::new(
        DistillationConfig::default(),
        embedder.clone(),
        store.clone(),
    );

    let problem = "How do I fix the connection refused error in the Rust server?";
    let solution = "Start the server before connecting, or bind to the correct port.";
    let content = format!("Problem: {problem} → Solution: {solution}");

    let mut existing = Experience::new("t1", MemoryType::Knowledge, &content, 0.99);
    existing.user_id = "alice".into();
    store.create(&existing).await.expect("seed");

    let messages = vec![
        Message::new("user", problem),
        Message::new("assistant", solution),
    ];
    let _out = distiller
        .distill("c3", &messages, "t1", "bob")
        .await
        .expect("distill");

    let count = store.count_for_tenant("t1").await.expect("count");
    assert_eq!(
        count, 2,
        "keyword dedup must not drop bob's candidate or delete alice's row"
    );
    let left = store
        .get_by_memory_type("t1", MemoryType::Knowledge)
        .await
        .expect("get");
    assert!(
        left.iter().any(|e| e.user_id == "alice"),
        "alice's row must still exist"
    );
    assert!(
        left.iter().any(|e| e.user_id == "bob"),
        "bob's distilled memory must be persisted"
    );
}
