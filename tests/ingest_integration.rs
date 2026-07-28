//! Integration tests for the character knowledge graph ingestion pipeline.
//!
//! Exercises the full [`IngestionPipeline`] against real corpus files
//! (水浒传, 三国演义, 红楼梦, 西游记) and a synthetic mini-corpus, verifying
//! character extraction, event creation, relation detection, and network
//! traversal.
//!
//! ## Test layout
//!
//! | Test | Speed | Default? |
//! |-----|-------|----------|
//! | `pipeline_ingests_synthetic_corpus` | Fast (~50ms) | Yes |
//! | `real_corpus_*` | Slow (~30-60s) | No (`#[ignore]`) |
//!
//! The slow tests share a single pipeline run via a `OnceCell` fixture so
//! the real corpus is only distilled once across all `#[ignore]` tests.

use std::path::Path;
use std::sync::Arc;

use lore_scope::character::{CharacterStore, SQLiteCharacterStore, traverse_character_network};
use lore_scope::ingest::{IngestionPipeline, IngestionStats};
use tokio::sync::OnceCell;

/// Tenant ID used by the ingestion pipeline for all novel data.
const TENANT: &str = "novels";

// ============================================================================
// Shared fixture for slow tests (real corpus)
// ============================================================================

/// Cached result of running the pipeline against the real `corpus/` directory.
/// Built once; all `#[ignore]` tests share the same store + stats.
struct CorpusFixture {
    store: Arc<SQLiteCharacterStore>,
    stats: IngestionStats,
}

/// Global once-cell so the 30-60s pipeline run happens exactly once per
/// test binary invocation, regardless of how many slow tests execute.
static CORPUS_FIXTURE: OnceCell<CorpusFixture> = OnceCell::const_new();

/// Return the shared corpus fixture, initializing it on first call.
async fn corpus_fixture() -> &'static CorpusFixture {
    CORPUS_FIXTURE
        .get_or_init(|| async {
            let store = Arc::new(
                SQLiteCharacterStore::open_in_memory()
                    .await
                    .expect("open in-memory character store for corpus fixture"),
            );
            let pipeline = IngestionPipeline::new(store.clone(), "corpus");
            let stats = pipeline
                .run()
                .await
                .expect("run ingestion pipeline on real corpus");
            CorpusFixture { store, stats }
        })
        .await
}

// ============================================================================
// Fast test — runs by default (no #[ignore])
// ============================================================================

/// Write a single-chapter synthetic novel file into `dir`.
/// The chapter marker `第一回` is required for `split_into_chapters` to
/// recognize it, and `body` must contain a character name plus a strong
/// verb so the pipeline extracts at least one event.
fn write_synthetic_novel(dir: &Path, filename: &str, body: &str) {
    let content = format!("第一回 测试章节\n{body}\n");
    std::fs::write(dir.join(filename), content)
        .unwrap_or_else(|e| panic!("write synthetic novel {filename}: {e}"));
}

/// Objective: Verify the pipeline ingests a small synthetic corpus and
/// extracts at least one character and one event per novel.
/// Invariants:
/// - The temp directory is isolated from the real corpus.
/// - The in-memory store starts empty and contains >= 4 characters after run.
/// - 宋江 is searchable by exact name in 水浒传.
/// - 宋江 has at least one event (text contains the strong verb "杀").
#[tokio::test]
async fn pipeline_ingests_synthetic_corpus() {
    let tmp = tempfile::tempdir().expect("create temp dir for synthetic corpus");

    // One chapter per novel; each body contains a known character and a
    // strong verb so event extraction succeeds.
    write_synthetic_novel(tmp.path(), "水浒传.txt", "宋江怒杀阎婆惜，逃往梁山泊。");
    write_synthetic_novel(
        tmp.path(),
        "三国演义.txt",
        "刘备、关羽、张飞三人结义为兄弟，誓同生死。",
    );
    write_synthetic_novel(tmp.path(), "红楼梦.txt", "贾宝玉笑道：这个妹妹我曾见过的。");
    write_synthetic_novel(tmp.path(), "西游记.txt", "孙悟空大闹天宫，被佛祖镇压。");

    let store = Arc::new(
        SQLiteCharacterStore::open_in_memory()
            .await
            .expect("open in-memory store for synthetic test"),
    );
    let pipeline = IngestionPipeline::new(store.clone(), tmp.path().to_str().unwrap());
    let stats = pipeline
        .run()
        .await
        .expect("ingest synthetic corpus without errors");

    // Four novels, each with at least one known character.
    assert!(
        stats.characters >= 4,
        "expected >= 4 characters from synthetic corpus, got {}",
        stats.characters
    );

    // 宋江 should be searchable by exact name in 水浒传.
    let results = store
        .search_characters_by_name("宋江", TENANT, Some("水浒传"))
        .await
        .expect("search 宋江 in synthetic corpus");
    assert!(
        !results.is_empty(),
        "宋江 should be found in 水浒传 after synthetic ingestion"
    );

    // 宋江's chapter text contains "杀" (a strong verb), so at least one
    // event should have been created.
    let events = store
        .get_character_events("宋江", TENANT, Some("水浒传"))
        .await
        .expect("get 宋江 events from synthetic corpus");
    assert!(
        !events.is_empty(),
        "宋江 should have >= 1 event from synthetic corpus (text has strong verb 杀)"
    );
}

/// Objective: Verify the pipeline persists **dimensional scoring** metadata on
/// each `CharacterRelation`, so downstream consumers (MCP tools, network
/// traversal) can explain *why* a relation scored the way it did.
///
/// Invariants:
/// - 刘备 and 关羽 co-occur in >= 3 chapters (relation threshold met).
/// - At least one relation involves 关羽 and is typed "结义" (cross-chapter
///   best-type tracking finds the oath keyword in chapter 1).
/// - The relation's `metadata` JSON bag contains all three score dimensions:
///   `co_occurrence_score`, `event_coupling_score`, `relation_type_score`.
/// - `importance` is the weighted combination of the three scores, in [0, 1].
/// - A typed relation (结义) yields `relation_type_score == 1.0`.
#[tokio::test]
async fn pipeline_persists_dimensional_scores_on_relations() {
    let tmp = tempfile::tempdir().expect("create temp dir for dimensional scoring test");

    // Three chapters where 刘备 and 关羽 co-occur. Chapter 1 carries the
    // oath-of-brotherhood keyword so the cross-chapter tracker should lock
    // in "结义" instead of falling back to generic "关联".
    let content = "\
第一回 宴桃园豪杰三结义
刘备、关羽、张飞三人结义为兄弟，誓同生死。
第二回 讨伐黄巾
刘备与关羽同破黄巾贼，大胜而归。
第三回 任平原相
刘备任平原相，关羽为马弓手，张飞为步弓手。
";
    std::fs::write(tmp.path().join("三国演义.txt"), content).expect("write multi-chapter 三国演义");
    // `pipeline.run()` iterates all four novels; create empty files for the
    // other three so `load_novel` returns empty chapter lists (skipped).
    for empty in &["水浒传.txt", "红楼梦.txt", "西游记.txt"] {
        std::fs::write(tmp.path().join(empty), "").expect("write empty novel file");
    }

    let store = Arc::new(
        SQLiteCharacterStore::open_in_memory()
            .await
            .expect("open in-memory store for dimensional scoring test"),
    );
    let pipeline = IngestionPipeline::new(store.clone(), tmp.path().to_str().unwrap());
    let _stats = pipeline
        .run()
        .await
        .expect("ingest multi-chapter novel without errors");

    let rels = store
        .get_relations_for_character("刘备", TENANT, Some("三国演义"))
        .await
        .expect("get 刘备 relations");

    let guanyu_rel = rels
        .iter()
        .find(|r| r.source_character == "关羽" || r.target_character == "关羽")
        .expect("刘备 should have a relation involving 关羽");

    assert_eq!(
        guanyu_rel.relation_type, "结义",
        "cross-chapter tracker should lock in 结义 from chapter 1's oath text"
    );

    // === Dimensional scoring metadata ===
    let md = &guanyu_rel.metadata;
    assert!(
        !md.is_empty(),
        "relation metadata should be populated with dimensional scores"
    );
    let has =
        |k: &str| -> bool { md.entries.contains_key(k) || md.entries.contains_key(&k.to_string()) };
    assert!(
        has("co_occurrence_score"),
        "metadata should contain co_occurrence_score, got: {:?}",
        md.entries.keys().collect::<Vec<_>>()
    );
    assert!(
        has("event_coupling_score"),
        "metadata should contain event_coupling_score"
    );
    assert!(
        has("relation_type_score"),
        "metadata should contain relation_type_score"
    );

    // A typed relation should yield the maximum type score.
    let type_score = md
        .entries
        .get("relation_type_score")
        .and_then(|v| v.as_f64())
        .expect("relation_type_score should be a number");
    assert_eq!(
        type_score, 1.0,
        "typed relation (结义) should have relation_type_score == 1.0"
    );

    // Combined importance is a weighted sum in [0, 1].
    assert!(
        (0.0..=1.0).contains(&guanyu_rel.importance),
        "importance should be in [0, 1], got {}",
        guanyu_rel.importance
    );
}

// ============================================================================
// Slow tests — marked #[ignore], run with: cargo test -- --ignored
// ============================================================================

/// Objective: Verify the pipeline extracts characters from all four novels
/// in the real corpus.
/// Invariants: Each novel must yield at least one character (count > 0).
#[tokio::test]
#[ignore]
async fn real_corpus_extracts_characters_per_novel() {
    let fixture = corpus_fixture().await;

    for novel in &["水浒传", "三国演义", "红楼梦", "西游记"] {
        let count = fixture
            .store
            .count_characters(TENANT, Some(novel))
            .await
            .expect("count characters");
        assert!(
            count > 0,
            "novel {novel} should have >= 1 character, got {count}"
        );
    }
}

/// Objective: Verify specific well-known characters exist in each novel.
/// Invariants: 宋江 in 水浒传, 刘备 in 三国演义, 贾宝玉 in 红楼梦,
/// 孙悟空 in 西游记 — each must be found by exact name search.
#[tokio::test]
#[ignore]
async fn real_corpus_known_characters_exist() {
    let fixture = corpus_fixture().await;

    let cases = [
        ("宋江", "水浒传"),
        ("刘备", "三国演义"),
        ("贾宝玉", "红楼梦"),
        ("孙悟空", "西游记"),
    ];

    for (name, novel) in &cases {
        let results = fixture
            .store
            .search_characters_by_name(name, TENANT, Some(novel))
            .await
            .expect("search character by name");
        assert!(
            !results.is_empty(),
            "{name} should exist in {novel} after real corpus ingestion"
        );
    }
}

/// Objective: Verify events were created across the real corpus.
/// Invariants: Total event count > 0; each novel has at least one event.
#[tokio::test]
#[ignore]
async fn real_corpus_events_created() {
    let fixture = corpus_fixture().await;

    assert!(
        fixture.stats.events > 0,
        "pipeline should create events from real corpus, got {}",
        fixture.stats.events
    );

    for novel in &["水浒传", "三国演义", "红楼梦", "西游记"] {
        let count = fixture
            .store
            .count_events(TENANT, Some(novel))
            .await
            .expect("count events");
        assert!(
            count > 0,
            "novel {novel} should have >= 1 event, got {count}"
        );
    }
}

/// Objective: Verify relations were created across the real corpus.
/// Invariants: Total relation count > 0 (co-occurrence threshold met by
/// major characters across many chapters).
#[tokio::test]
#[ignore]
async fn real_corpus_relations_created() {
    let fixture = corpus_fixture().await;

    assert!(
        fixture.stats.relations > 0,
        "pipeline should create relations from real corpus, got {}",
        fixture.stats.relations
    );
}

/// Objective: Verify a specific known relation exists between 刘备 and 关羽
/// in 三国演义 — their famous oath of brotherhood (结义).
/// Invariants:
/// - 刘备 has at least one relation in 三国演义.
/// - At least one relation involves 关羽 (as source or target).
/// - At least one such relation has type "结义".
#[tokio::test]
#[ignore]
async fn real_corpus_liubei_guanyu_jieyi_relation() {
    let fixture = corpus_fixture().await;

    let rels = fixture
        .store
        .get_relations_for_character("刘备", TENANT, Some("三国演义"))
        .await
        .expect("get 刘备 relations");

    assert!(
        !rels.is_empty(),
        "刘备 should have >= 1 relation in 三国演义"
    );

    // Filter to relations that involve 关羽 on either side.
    let guanyu_rels: Vec<_> = rels
        .iter()
        .filter(|r| r.source_character == "关羽" || r.target_character == "关羽")
        .collect();

    assert!(
        !guanyu_rels.is_empty(),
        "刘备 should have a relation involving 关羽 in 三国演义"
    );

    // The first chapter of 三国演义 describes their oath of brotherhood,
    // so at least one relation should be typed "结义".
    let has_jieyi = guanyu_rels.iter().any(|r| r.relation_type == "结义");
    assert!(
        has_jieyi,
        "刘备 and 关羽 should have a 结义 relation, found types: {:?}",
        guanyu_rels
            .iter()
            .map(|r| &r.relation_type)
            .collect::<Vec<_>>()
    );
}

/// Objective: Verify `traverse_character_network` works on real corpus data
/// and returns a connected subgraph for 宋江.
/// Invariants:
/// - The root node's character name is "宋江".
/// - The node has at least one event.
/// - The node has at least one relation.
/// - The node has at least one connected character at depth 2.
#[tokio::test]
#[ignore]
async fn real_corpus_character_network_traversal() {
    let fixture = corpus_fixture().await;

    let node = traverse_character_network(&*fixture.store, "宋江", TENANT, Some("水浒传"), 2)
        .await
        .expect("traverse 宋江 network at depth 2");

    assert_eq!(
        node.character.name, "宋江",
        "root node character should be 宋江"
    );
    assert!(
        !node.events.is_empty(),
        "宋江 should have events in the network"
    );
    assert!(
        !node.relations.is_empty(),
        "宋江 should have relations in the network"
    );
    assert!(
        !node.connections.is_empty(),
        "宋江 should have >= 1 connected character at depth 2"
    );
}
