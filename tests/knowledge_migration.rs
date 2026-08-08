//! Integration tests for the V1 → general knowledge model migration.
//!
//! Verifies the Phase 0 acceptance test from `docs/zh/dev_guide.md` §7:
//!
//! > `inspect_entity("赵云")` 跑通，返回 Object + Edges + Evidence.
//!
//! ## Test layout
//!
//! | Test | Speed | Default? |
//! |-----|-------|----------|
//! | `synthetic_corpus_migrates_and_inspects` | Fast (~80ms) | Yes |
//! | `real_corpus_inspect_zhaoyun_acceptance` | Slow (~30-60s) | No (`#[ignore]`) |
//!
//! The slow test mirrors `tests/ingest_integration.rs`: a `OnceCell` fixture
//! runs the ingestion pipeline + migrator exactly once across all `#[ignore]`
//! tests, so the real corpus is distilled + migrated a single time per binary
//! invocation.

use std::path::Path;
use std::sync::Arc;

use mnemosyne::character::SQLiteCharacterStore;
use mnemosyne::ingest::IngestionPipeline;
use mnemosyne::knowledge::{KnowledgeStore, Migrator, SQLiteKnowledgeStore};

// ============================================================================
// Fast test — runs by default (no #[ignore])
// ============================================================================

/// Write a two-chapter 三国演义 corpus into `dir` with 赵云 present in both
/// chapters so the V1 pipeline extracts him, an event, and a relation.
fn write_synthetic_three_kingdoms(dir: &Path) {
    // Two chapters so co-occurrence detection has material to work with.
    // Chapter 1 introduces 刘备+关羽+张飞 with the 结义 keyword; chapter 2
    // has 赵云 rescue 阿斗 with a strong verb so event extraction fires.
    let content = "\
第一回 宴桃园豪杰三结义
刘备、关羽、张飞三人结义为兄弟，誓同生死。
第二回 赵子龙单骑救主
赵云单骑救阿斗，杀透重围，刘备大喜称其忠勇。
";
    std::fs::write(dir.join("三国演义.txt"), content)
        .unwrap_or_else(|e| panic!("write synthetic 三国演义: {e}"));
    // Empty files for the other novels so load_novel returns empty (skipped).
    for empty in &["水浒传.txt", "红楼梦.txt", "西游记.txt"] {
        std::fs::write(dir.join(empty), "")
            .unwrap_or_else(|e| panic!("write empty novel {empty}: {e}"));
    }
}

/// Objective: Verify the end-to-end Phase 0 flow on a synthetic corpus —
/// V1 ingest → migrate → `inspect_entity("赵云")` returns Object + Edges +
/// Evidence.
///
/// Invariants:
/// - Migration produces >= 1 document, 2 chapters, >= 4 objects (>= 3 persons
///   + 1 event for 赵云's rescue), and >= 1 evidence row.
/// - `inspect_entity("赵云", Some("三国演义"))` returns Some with object.name
///   == "赵云".
/// - The returned entity has >= 1 evidence row whose content references the
///   rescue (contains "阿斗" or "赵云").
/// - The returned entity has >= 1 mention in the corpus.
#[tokio::test]
async fn synthetic_corpus_migrates_and_inspects() {
    let tmp = tempfile::tempdir().expect("create temp dir for synthetic migration");

    // 1. V1 ingest against the synthetic corpus.
    write_synthetic_three_kingdoms(tmp.path());
    let v1 = Arc::new(
        SQLiteCharacterStore::open_in_memory()
            .await
            .expect("open V1 in-memory store"),
    );
    let pipeline = IngestionPipeline::new(v1.clone(), tmp.path().to_str().unwrap());
    let v1_stats = pipeline.run().await.expect("ingest synthetic corpus");
    assert!(
        v1_stats.characters >= 3,
        "synthetic corpus should yield >= 3 characters, got {}",
        v1_stats.characters
    );

    // 2. Migrate V1 → general knowledge model.
    let knowledge = Arc::new(
        SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("open knowledge in-memory store"),
    );
    let migrator = Migrator::new(&*v1, &knowledge, tmp.path());
    let stats = migrator
        .migrate()
        .await
        .expect("migrate synthetic V1 → general");

    assert_eq!(
        stats.documents, 1,
        "one novel (三国演义) should be migrated, got {}",
        stats.documents
    );
    assert_eq!(stats.chapters, 2, "two chapters in synthetic corpus");
    assert!(
        stats.objects >= 4,
        ">= 3 persons + 1 event, got {}",
        stats.objects
    );
    assert!(
        stats.evidence > 0,
        "migration should produce evidence rows, got {}",
        stats.evidence
    );
    assert!(
        stats.mentions > 0,
        "corpus mentions characters, got {} mentions",
        stats.mentions
    );

    // 3. Phase 0 acceptance: inspect_entity("赵云").
    let zhaoyun = knowledge
        .inspect_entity("赵云", Some("三国演义"))
        .await
        .expect("inspect 赵云")
        .expect("赵云 should exist in the general model after migration");

    assert_eq!(zhaoyun.object.name, "赵云");
    assert!(
        !zhaoyun.evidences.is_empty(),
        "赵云 must have backing evidence after migration"
    );
    assert!(
        zhaoyun
            .evidences
            .iter()
            .any(|e| e.content.contains("阿斗") || e.content.contains("赵云")),
        "at least one evidence should reference the rescue text"
    );
    assert!(
        !zhaoyun.mentions.is_empty(),
        "赵云 should be mentioned in the corpus"
    );
}

/// Objective: Verify migration is idempotent — a second `migrate()` call on
/// the same stores completes without error and does not corrupt the
/// previously-migrated data.
///
/// Invariants: second migrate() returns Ok; `inspect_entity("赵云")` still
/// resolves after the second run.
#[tokio::test]
async fn migration_is_idempotent_on_synthetic_corpus() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_synthetic_three_kingdoms(tmp.path());
    let v1 = Arc::new(SQLiteCharacterStore::open_in_memory().await.expect("v1"));
    let pipeline = IngestionPipeline::new(v1.clone(), tmp.path().to_str().unwrap());
    pipeline.run().await.expect("ingest");
    let knowledge = SQLiteKnowledgeStore::open_in_memory().await.expect("k");

    let migrator = Migrator::new(&*v1, &knowledge, tmp.path());
    let _ = migrator.migrate().await.expect("first migrate");
    // Second run must not error — documents are reused via find_document_by_title.
    let _ = migrator.migrate().await.expect("second migrate is safe");

    let zhaoyun = knowledge
        .inspect_entity("赵云", Some("三国演义"))
        .await
        .expect("inspect after re-migrate")
        .expect("赵云 still resolvable");
    assert_eq!(zhaoyun.object.name, "赵云");
}
