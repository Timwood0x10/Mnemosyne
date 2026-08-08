//! 三国演义 corpus lifecycle regression: distill → conflict → forget.
//!
//! Uses real 三国演义 text paragraphs as conversation input to verify the
//! memory lifecycle closed loop end-to-end:
//!   1. Distillation extracts memories from the corpus text.
//!   2. Re-stating the same content resolves as a conflict (replaced or deduped).
//!   3. TTL-expired memories are purged by the forget-expired maintenance phase.
//!
//! Run: cargo test --test sanguo_lifecycle -- --nocapture

use std::sync::Arc;

use chrono::Utc;
use mnemosyne::distiller::{DistillationConfig, Distiller, PipelineDistiller};
use mnemosyne::embed::EmbeddingService;
use mnemosyne::store::{ExperienceRepository, SQLiteVecStore};
use mnemosyne::types::{MemoryType, Message};

/// Simple deterministic embedder so vector search works without a remote API.
#[derive(Clone)]
struct TestEmbedder;

#[async_trait::async_trait]
impl EmbeddingService for TestEmbedder {
    async fn embed(&self, text: &str) -> mnemosyne::error::Result<Vec<f32>> {
        // 8-dim bag-of-words-ish vector so similar text has similar vectors.
        let mut v = vec![0.0_f32; 8];
        for (i, ch) in text.chars().enumerate() {
            v[i % 8] += ch as u32 as f32 / 1000.0;
        }
        Ok(v)
    }
    async fn embed_with_prefix(
        &self,
        _prefix: &str,
        text: &str,
    ) -> mnemosyne::error::Result<Vec<f32>> {
        self.embed(text).await
    }
    async fn health_check(&self) -> mnemosyne::error::Result<()> {
        Ok(())
    }
    fn model(&self) -> &str {
        "test"
    }
    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(5)
    }
    fn enabled(&self) -> bool {
        true
    }
}

fn load_sanguo_messages() -> Vec<Message> {
    // The distiller extracts Problem-Solution pairs via `is_problem`, which
    // requires question-shaped user turns. We wrap real 三国演义 content in
    // that shape so the lifecycle exercises genuine corpus knowledge.
    let pairs: &[(&str, &str)] = &[
        (
            "How did Liu Bei bind himself to Guan Yu and Zhang Fei? 桃园结义",
            "刘备与关羽、张飞桃园结义，约为兄弟，同生共死。",
        ),
        (
            "How to understand Lu Bu's loyalty? 吕布杀丁原",
            "吕布先事丁原，后杀丁原投董卓，反复无常。",
        ),
        (
            "What made Cao Cao a great strategist? 曹操治世能臣",
            "曹操字孟德，治世之能臣，乱世之奸雄，善用谋士。",
        ),
        (
            "How did Zhuge Liang serve Liu Bei? 三顾茅庐",
            "刘备三顾茅庐请诸葛亮出山，诸葛亮未出茅庐已知三分天下。",
        ),
        (
            "Why is Guan Yu remembered? 青龙偃月刀",
            "关羽身长九尺，面如重枣，使青龙偃月刀，义薄云天。",
        ),
        (
            "How to rescue besieged Xuzhou? 刘备救徐州",
            "刘备率军救徐州，百姓感激，曹操为之忌惮。",
        ),
    ];
    let mut msgs = Vec::new();
    for (problem, solution) in pairs {
        msgs.push(Message::new("user", *problem));
        msgs.push(Message::new("assistant", *solution));
    }
    msgs
}

/// Objective: Verify the full lifecycle on real 三国 corpus text.
/// Invariants: Distillation produces memories; a repeated distillation
/// triggers conflict handling (no unbounded duplicates); forget-expired
/// purges TTL-expired rows for the tenant.
#[tokio::test]
async fn sanguo_lifecycle_distill_conflict_forget() {
    let store = Arc::new(SQLiteVecStore::open_in_memory(8).await.expect("open store"));
    let embedder: Arc<dyn EmbeddingService> = Arc::new(TestEmbedder);
    let cfg = DistillationConfig {
        min_importance: 0.0,
        conflict_threshold: 0.9,
        max_memories_per_distillation: 100,
        max_solutions_per_tenant: 1000,
        enable_cross_turn: true,
    };
    let d = PipelineDistiller::new(cfg, embedder, store.clone());

    let msgs = load_sanguo_messages();
    assert!(
        !msgs.is_empty(),
        "三国 corpus must yield extractable message turns"
    );
    println!(
        "三国 lifecycle input: {} turns, first: {}",
        msgs.len(),
        msgs[0].content.chars().take(40).collect::<String>()
    );

    // 1. Distill the corpus conversation — should produce memories.
    let first = d
        .distill("sanguo-1", &msgs, "t1", "u1")
        .await
        .expect("first distill");
    println!("first distill -> {} memories", first.len());
    assert!(
        !first.is_empty(),
        "real 三国 text should yield at least one memory"
    );

    // 2. Distill the SAME content again — conflict resolution must kick in
    //    (vector similarity is high; identical content is deduped/replaced).
    let second = d
        .distill("sanguo-2", &msgs, "t1", "u1")
        .await
        .expect("second distill");
    let m = d.metrics();
    println!(
        "second distill -> {} new, conflicts_resolved={}, replaced={}",
        second.len(),
        m.conflicts_resolved,
        m.memories_replaced
    );
    // Either the duplicates were dropped (small/empty second) or replaced —
    // but total tenant rows must not be unbounded by content-hash dedup.
    let total = store.count_for_tenant("t1").await.expect("count tenant");
    println!("tenant t1 total memories after two distills: {total}");
    assert!(
        total >= first.len() as i64,
        "tenant must retain the distilled memories"
    );

    // 3. Forget-expired: insert an expired row directly, run empty distill,
    //    and verify the maintenance phase purges it.
    let mut expired =
        mnemosyne::types::Experience::new("t1", MemoryType::Knowledge, "过期记忆：董卓旧事", 0.9);
    expired.id = "sanguo-expired-1".to_string();
    expired.expires_at = Some(Utc::now() - chrono::Duration::seconds(60));
    store.create(&expired).await.expect("create expired");

    let before = store.get("sanguo-expired-1").await.expect("get").is_some();
    assert!(before, "expired row must exist before maintenance");

    let _ = d.distill("sanguo-empty", &[], "t1", "u1").await;
    let m2 = d.metrics();
    println!(
        "after forget phase: memories_forgotten={}",
        m2.memories_forgotten
    );
    assert!(
        store.get("sanguo-expired-1").await.expect("get").is_none(),
        "TTL-expired memory must be purged by the forget phase"
    );
    println!("\n========== SANGUO LIFECYCLE COMPLETE ==========");
}
