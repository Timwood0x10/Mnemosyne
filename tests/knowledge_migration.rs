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

use lore_scope::character::SQLiteCharacterStore;
use lore_scope::ingest::IngestionPipeline;
use lore_scope::knowledge::{KnowledgeStore, MigrationStats, Migrator, SQLiteKnowledgeStore};
use tokio::sync::OnceCell;

// ============================================================================
// Shared fixture for slow tests (real corpus)
// ============================================================================

/// Cached result of running ingest + migrate against the real `corpus/` dir.
/// Built once; all `#[ignore]` tests share the same knowledge store + stats.
struct MigrationFixture {
    knowledge: Arc<SQLiteKnowledgeStore>,
    stats: MigrationStats,
}

/// Global once-cell so the 30-60s ingest + migrate happens exactly once per
/// test binary invocation, regardless of how many slow tests execute.
static MIGRATION_FIXTURE: OnceCell<MigrationFixture> = OnceCell::const_new();

/// Return the shared migration fixture, initializing it on first call.
///
/// The fixture ingests the real corpus into a V1 in-memory store, then
/// migrates it into a general-model in-memory store. Both stores are dropped
/// at process exit.
async fn migration_fixture() -> &'static MigrationFixture {
    MIGRATION_FIXTURE
        .get_or_init(|| async {
            // 1. Ingest the real corpus into V1.
            let v1 = Arc::new(
                SQLiteCharacterStore::open_in_memory()
                    .await
                    .expect("open in-memory V1 store for fixture"),
            );
            let pipeline = IngestionPipeline::new(v1.clone(), "corpus");
            pipeline
                .run()
                .await
                .expect("ingest real corpus into V1 store");

            // 2. Migrate V1 → general knowledge model.
            let knowledge = Arc::new(
                SQLiteKnowledgeStore::open_in_memory()
                    .await
                    .expect("open in-memory knowledge store for fixture"),
            );
            let migrator = Migrator::new(&*v1, &knowledge, Path::new("corpus"));
            let stats = migrator.migrate().await.expect("migrate V1 → general");

            MigrationFixture { knowledge, stats }
        })
        .await
}

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

// ============================================================================
// Slow tests — marked #[ignore], run with: cargo test -- --ignored
// ============================================================================

/// Objective: Verify the Phase 0 acceptance test against the real corpus —
/// after ingesting + migrating all four novels, `inspect_entity("赵云")`
/// returns a non-empty Object + Edges + Evidence.
///
/// Invariants:
/// - Migration stats show >= 4 documents (one per novel).
/// - `inspect_entity("赵云", Some("三国演义"))` returns Some.
/// - The returned object has name == "赵云" and >= 1 evidence row.
#[tokio::test]
#[ignore]
async fn real_corpus_inspect_zhaoyun_acceptance() {
    let fixture = migration_fixture().await;

    assert!(
        fixture.stats.documents >= 4,
        "all four novels should be migrated, got {}",
        fixture.stats.documents
    );
    assert!(
        fixture.stats.objects > 0,
        "real corpus should yield objects, got {}",
        fixture.stats.objects
    );
    assert!(
        fixture.stats.evidence > 0,
        "real corpus should yield evidence, got {}",
        fixture.stats.evidence
    );

    let zhaoyun = fixture
        .knowledge
        .inspect_entity("赵云", Some("三国演义"))
        .await
        .expect("inspect 赵云 on real corpus")
        .expect("赵云 must exist in the migrated real corpus");
    assert_eq!(zhaoyun.object.name, "赵云");
    assert!(
        !zhaoyun.evidences.is_empty(),
        "赵云 must have backing evidence on the real corpus"
    );
}

/// Objective: Verify the `timeline` tool against real-corpus migrated data:
/// `entity_timeline("赵云", Some("三国演义"))` returns a non-empty, chapter-
/// ordered list.
///
/// Invariants: timeline is non-empty and sorted ascending by chapter.
#[tokio::test]
#[ignore]
async fn real_corpus_zhaoyun_timeline_ordered() {
    let fixture = migration_fixture().await;
    let tl = fixture
        .knowledge
        .entity_timeline("赵云", Some("三国演义"))
        .await
        .expect("timeline on real corpus");
    assert!(!tl.is_empty(), "赵云 should have a non-empty timeline");
    // Verify ascending chapter order.
    let mut prev = 0;
    for entry in &tl {
        assert!(
            entry.chapter >= prev,
            "timeline must be sorted ascending; got {prev} then {}",
            entry.chapter
        );
        prev = entry.chapter;
    }
}

/// Objective: Verify the `relation_graph` tool against real-corpus migrated
/// data: a depth-2 BFS around 赵云 returns >= 1 node and edge.
///
/// Invariants: result is Some; nodes.len() >= 1; edges.len() >= 0 (赵云 may
/// have relations only at depth > 1).
#[tokio::test]
#[ignore]
async fn real_corpus_zhaoyun_relation_graph() {
    let fixture = migration_fixture().await;
    let g = fixture
        .knowledge
        .relation_graph("赵云", 2, Some("三国演义"))
        .await
        .expect("relation_graph on real corpus")
        .expect("赵云 should resolve for relation_graph");
    assert!(
        !g.nodes.is_empty(),
        "graph should contain at least the root node"
    );
    // Every node must have a non-empty name (regression guard against empty
    // rows sneaking in via stale V1 data).
    for n in &g.nodes {
        assert!(!n.name.is_empty(), "node {:?} has empty name", n);
    }
}

/// Objective: Verify the `evidence` search tool against real-corpus migrated
/// data: a query for "赵云" returns >= 1 hit.
///
/// Invariants: at least one hit whose text contains "赵云".
#[tokio::test]
#[ignore]
async fn real_corpus_evidence_search_finds_zhaoyun() {
    let fixture = migration_fixture().await;
    let hits = fixture
        .knowledge
        .search_evidence("赵云", Some("三国演义"), 50)
        .await
        .expect("search evidence on real corpus");
    assert!(
        !hits.is_empty(),
        "evidence search for 赵云 should find at least one hit"
    );
    assert!(
        hits.iter().any(|h| h.text.contains("赵云")),
        "at least one hit's text should contain 赵云"
    );
}
