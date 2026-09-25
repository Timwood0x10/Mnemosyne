//! Knowledge-store unit tests: CRUD, transactions, search and world model.

use super::*;
use serde_json::json;

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

async fn fresh() -> SQLiteKnowledgeStore {
    SQLiteKnowledgeStore::open_in_memory().await.expect("open")
}

/// Objective: Verify the P2 fix — `upsert_world_entity` is atomic and
/// unique-by-name is DB-enforced. Two INDEPENDENT store instances sharing
/// the same SQLite file must not create duplicate rows when they upsert the
/// same name concurrently.
/// Invariants: after racing two instances on one name, exactly ONE
/// `world_entities` row exists with that name.
#[tokio::test]
async fn concurrent_upsert_does_not_duplicate() {
    // Unique path per run: a fixed name raced with parallel cargo-test
    // processes on the same machine (two binaries / nextest workers) and
    // intermittently failed the uniqueness assertion.
    let path = std::env::temp_dir().join(format!(
        "lorescope_p2_dup_{}_{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&path);
    // Two independent connections to the SAME file — each has its own
    // mutex, so the pre-fix check-then-insert could both pass the SELECT.
    let store_a = SQLiteKnowledgeStore::open(path.to_str().unwrap())
        .await
        .expect("open a");
    let store_b = SQLiteKnowledgeStore::open(path.to_str().unwrap())
        .await
        .expect("open b");

    let (id_a, id_b) = tokio::join!(
        store_a.upsert_world_entity("诸葛亮", "person", 0.9),
        store_b.upsert_world_entity("诸葛亮", "person", 0.9),
    );
    assert!(id_a.is_ok(), "first upsert ok");
    assert!(
        id_b.is_ok(),
        "second upsert ok — conflict must be handled, not errored"
    );

    // Count rows for this name — must be exactly one.
    let count: i64 = store_a
        .conn
        .lock()
        .await
        .query_row(
            "SELECT COUNT(*) FROM world_entities WHERE name = ?1",
            params!["诸葛亮"],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "P2: concurrent upsert must not duplicate rows");

    let _ = std::fs::remove_file(&path);
}

/// Objective: Verify `WORLD_SCHEMA` (V7 entity-centric tables) is now
/// executed by `init` — the dead-code wiring fix for C10.
/// Invariants: after `open_in_memory`, the V7 `world_`-prefixed tables and
/// `events` exist (a fresh connection that never executed WORLD_SCHEMA
/// would fail this query). The prefix isolates the world model from the
/// fact-store's bare `entities` table sharing the same database file.
#[tokio::test]
async fn world_schema_tables_are_created() {
    let store = fresh().await;
    let conn = store.conn.lock().await;
    for table in [
        "world_entities",
        "world_entity_aliases",
        "world_entity_profiles",
        "world_relations",
        "events",
        "world_states",
    ] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(
            count, 1,
            "WORLD_SCHEMA must create table `{table}` on init (C10 wiring)"
        );
    }
}

async fn seed_doc(store: &SQLiteKnowledgeStore, title: &str) -> i64 {
    store
        .create_document(&Document {
            id: 0,
            title: title.to_string(),
            author: None,
            doc_type: Some("novel".into()),
            created_at: now_ts(),
        })
        .await
        .expect("create doc")
}

async fn seed_chapter(store: &SQLiteKnowledgeStore, doc_id: i64, no: i32) -> i64 {
    store
        .create_chapter(&Chapter {
            id: 0,
            doc_id,
            chapter_no: no,
            title: Some(format!("ch{no}")),
            content: format!("第{no}回正文"),
            start_offset: Some(0),
            end_offset: Some(10),
        })
        .await
        .expect("create chapter")
}

async fn seed_person(
    store: &SQLiteKnowledgeStore,
    doc_id: i64,
    name: &str,
    props: serde_json::Value,
) -> i64 {
    store
        .create_object(&KnowledgeObject {
            id: 0,
            doc_id,
            object_type: ObjectType::Person,
            name: name.to_string(),
            properties: props,
            confidence: 0.9,
            created_at: now_ts(),
        })
        .await
        .expect("create person")
}

/// Objective: Verify document + chapter round-trip and lookup-by-no.
/// Invariants: A created chapter is retrievable by (doc_id, chapter_no).
#[tokio::test]
async fn document_and_chapter_round_trip() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let cid = seed_chapter(&store, did, 3).await;
    let got = store
        .get_chapter_by_no(did, 3)
        .await
        .expect("get chapter")
        .expect("chapter exists");
    assert_eq!(got.id, cid);
    assert_eq!(got.chapter_no, 3);
}

/// Objective: Verify an explicit transaction COMMIT persists all writes
/// made since BEGIN (H6 — the migrator's atomicity relies on this).
/// Invariants: after commit, the created document is visible.
#[tokio::test]
async fn transaction_commit_persists_writes() {
    let store = fresh().await;
    store.begin_transaction().await.expect("begin");
    seed_doc(&store, "事务提交测试").await;
    store.commit_transaction().await.expect("commit");
    let doc = store
        .find_document_by_title("事务提交测试")
        .await
        .expect("query");
    assert!(
        doc.is_some(),
        "committed write must be visible after COMMIT"
    );
}

/// Objective: Verify an explicit transaction ROLLBACK discards every
/// write made since BEGIN (H6 — a failed migration must not leave a
/// half-migrated database).
/// Invariants: after rollback, the created document is NOT visible.
#[tokio::test]
async fn transaction_rollback_discards_writes() {
    let store = fresh().await;
    store.begin_transaction().await.expect("begin");
    seed_doc(&store, "事务回滚测试").await;
    store.rollback_transaction().await.expect("rollback");
    let doc = store
        .find_document_by_title("事务回滚测试")
        .await
        .expect("query");
    assert!(
        doc.is_none(),
        "rolled-back write must be invisible after ROLLBACK"
    );
}

/// Objective: Verify a failed store call inside a transaction leaves the
/// database unchanged when the transaction is rolled back (H6 partial-
/// failure scenario at the store level).
/// Invariants: doc + person written, then rollback → neither remains.
#[tokio::test]
async fn transaction_rollback_undoes_multi_row_writes() {
    let store = fresh().await;
    store.begin_transaction().await.expect("begin");
    let did = seed_doc(&store, "多行回滚测试").await;
    seed_person(
        &store,
        did,
        "关羽",
        serde_json::json!({ "novel": "三国演义" }),
    )
    .await;
    store.rollback_transaction().await.expect("rollback");
    assert!(
        store
            .find_document_by_title("多行回滚测试")
            .await
            .expect("query")
            .is_none(),
        "document must be rolled back"
    );
    let objects = store
        .list_objects_by_document(999_999)
        .await
        .expect("query");
    assert!(
        objects.is_empty(),
        "no objects should remain from the rolled-back transaction"
    );
}

/// Objective: Verify object CRUD, properties JSON round-trip, and
/// name-based lookup scoped to a doc.
/// Invariants: properties survive serialize/parse; find_object_by_name
/// returns None for a name that does not exist.
#[tokio::test]
async fn object_round_trip_and_lookup() {
    let store = fresh().await;
    let did = seed_doc(&store, "水浒传").await;
    let props = json!({"aliases": ["及时雨"], "importance": 0.8});
    let oid = seed_person(&store, did, "宋江", props.clone()).await;

    let got = store.get_object(oid).await.expect("get").expect("exists");
    assert_eq!(got.name, "宋江");
    assert_eq!(got.properties, props, "properties JSON must round-trip");

    let found = store
        .find_object_by_name("宋江", Some(did))
        .await
        .expect("find")
        .expect("found by name");
    assert_eq!(found.id, oid);

    let missing = store
        .find_object_by_name("不存在的角色", Some(did))
        .await
        .expect("find missing");
    assert!(missing.is_none(), "unknown name must return None");
}

/// Objective: Verify `find_object_by_alias` resolves a corpus-discovered
/// given name ("流苏") when queried by the full name ("白流苏"), and vice
/// versa — the companion-persona alias gap.
/// Invariants: full-name query finds the object stored under the given
/// name; exact-match precedence is preserved.
#[tokio::test]
async fn find_object_by_alias_matches_substring() {
    let store = fresh().await;
    let did = seed_doc(&store, "倾城之恋").await;
    seed_person(&store, did, "流苏", json!({})).await;

    // Query the full name → substring fallback finds the stored "流苏".
    let hit = store
        .find_object_by_alias("白流苏", Some(did))
        .await
        .expect("alias lookup")
        .expect("substring alias must resolve");
    assert_eq!(
        hit.name, "流苏",
        "full-name query resolves to the given-name node"
    );

    // Query the given name → substring fallback also matches.
    let hit2 = store
        .find_object_by_alias("苏", Some(did))
        .await
        .expect("alias lookup")
        .expect("shorter alias must resolve");
    assert_eq!(hit2.name, "流苏", "a shorter alias resolves too");
}

/// Objective: Verify `find_object_by_alias` prefers an exact match over a
/// substring hit, and never resolves an ambiguous substring to a wrong
/// entity.
/// Invariants: exact name wins; two objects sharing a substring yield None
/// (ambiguous); a wholly unknown name yields None.
#[tokio::test]
async fn find_object_by_alias_prefers_exact_and_rejects_ambiguous() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let liubei = seed_person(&store, did, "刘备", json!({})).await;
    seed_person(&store, did, "刘备用剑", json!({})).await;

    // Exact match is authoritative even though a substring alias exists.
    let exact = store
        .find_object_by_alias("刘备", Some(did))
        .await
        .expect("alias lookup")
        .expect("exact match must win");
    assert_eq!(
        exact.id, liubei,
        "exact name is returned, not the substring"
    );

    // A query that is a strict substring of the exact name still resolves
    // unambiguously to that object ("白流苏"-style: full name contains it).
    let strict = store
        .find_object_by_alias("刘备用剑", Some(did))
        .await
        .expect("alias lookup")
        .expect("strict substring of the stored name must resolve");
    assert_eq!(
        strict.name, "刘备用剑",
        "query contained by the name resolves"
    );

    // Wholly unknown → None.
    let unknown = store
        .find_object_by_alias("不存在的人", Some(did))
        .await
        .expect("alias lookup");
    assert!(unknown.is_none(), "unknown name resolves to None");
}

/// Objective: Verify `find_object_by_alias` returns None when two DIFFERENT
/// stored objects both match a substring — it must never guess which one
/// the caller meant.
/// Invariants: two objects sharing a query substring → None.
#[tokio::test]
async fn find_object_by_alias_rejects_ambiguous_substring() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    seed_person(&store, did, "赵云", json!({})).await;
    seed_person(&store, did, "赵飞", json!({})).await;

    // Both 赵云 and 赵飞 contain "赵" → ambiguous → None (no guessing).
    let ambiguous = store
        .find_object_by_alias("赵", Some(did))
        .await
        .expect("alias lookup");
    assert!(
        ambiguous.is_none(),
        "ambiguous substring must not silently pick one entity"
    );
}

/// Objective: Verify the no-doc SQL path uses the RAW query as the reverse
/// LIKE haystack. The escaped form (`foo\_bar`) was previously bound as ?3,
/// so backslashes became part of the searched text and a query containing
/// `_`/`%` failed to match a stored substring the Rust filter would accept
/// (e.g. query `foo_bar` vs stored `oo_ba`).
/// Invariants: a query with `_` still resolves an unambiguous stored
/// substring via the no-doc path; exact names remain preferred.
#[tokio::test]
async fn find_object_by_alias_no_doc_escapes_pattern_not_haystack() {
    let store = fresh().await;
    let did = seed_doc(&store, "underscore regression").await;
    seed_person(&store, did, "oo_ba", json!({})).await;

    // doc-scoped path (list + filter) already worked; the bug is the no-doc
    // SQL branch. Call with doc_id = None so the escaped haystack would be
    // used if still present.
    let hit = store
        .find_object_by_alias("foo_bar", None)
        .await
        .expect("alias lookup must not fail")
        .expect(
            "query `foo_bar` must still match stored `oo_ba` \
             (escaped haystack regression)",
        );
    assert_eq!(
        hit.name, "oo_ba",
        "reverse containment must search the raw query, not the escaped form"
    );
    let _ = did;
}

/// Objective: Verify `update_object_properties` merges new keys into an
/// existing object's properties (preserving old keys) and bumps confidence.
/// Invariants: old key survives; new key present; confidence updated;
/// returns the row count.
#[tokio::test]
async fn update_object_properties_merges_keys() {
    let store = fresh().await;
    let did = seed_doc(&store, "水浒传").await;
    let oid = seed_person(&store, did, "宋江", json!({"aliases": ["及时雨"]})).await;

    let n = store
        .update_object_properties(
            oid,
            &json!({"preference": "重义气", "relations": []}),
            Some(0.95),
        )
        .await
        .expect("update");
    assert_eq!(n, 1, "one row updated");

    let got = store.get_object(oid).await.expect("get").expect("exists");
    assert_eq!(got.properties["aliases"][0], "及时雨", "old key preserved");
    assert_eq!(got.properties["preference"], "重义气", "new key merged in");
    assert!(
        got.properties["relations"].is_array(),
        "relations array set"
    );
    assert_eq!(got.confidence, 0.95, "confidence bumped");
}

/// Objective: Verify `update_object_properties` on an unknown id is a
/// safe no-op (returns 0, no panic).
/// Invariants: unknown id → Ok(0).
#[tokio::test]
async fn update_object_properties_unknown_id_noop() {
    let store = fresh().await;
    let n = store
        .update_object_properties(999_999, &json!({"x": 1}), None)
        .await
        .expect("update");
    assert_eq!(n, 0, "unknown id → zero rows updated");
}

/// Objective: Verify a properties-only merge (`confidence=None`) does NOT
/// clobber the stored confidence with a hardcoded value — the previous
/// `unwrap_or(0.8)` silently rewrote any stored value on every merge.
/// Invariants: stored confidence 0.95 stays 0.95 after a None merge.
#[tokio::test]
async fn update_object_properties_none_preserves_confidence() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let oid = seed_person(&store, did, "关羽", json!({"aliases": ["云长"]})).await;
    // Establish a distinct stored confidence.
    store
        .update_object_properties(oid, &json!({"title": "汉寿亭侯"}), Some(0.95))
        .await
        .expect("seed confidence");

    // Properties-only merge: must keep 0.95, not reset to 0.8.
    store
        .update_object_properties(oid, &json!({"preference": "重义"}), None)
        .await
        .expect("merge properties only");
    let got = store.get_object(oid).await.expect("get").expect("exists");
    assert_eq!(
        got.confidence, 0.95,
        "None must preserve the stored confidence, got {}",
        got.confidence
    );
    assert_eq!(
        got.properties["preference"], "重义",
        "properties still merged"
    );
    assert_eq!(
        got.properties["title"], "汉寿亭侯",
        "previous property preserved"
    );
}

/// Objective: Verify `search_objects` filters by type, name substring,
/// property value, and document scope, and honors the limit.
/// Invariants: type=person → only persons; query=张 → 张三;
/// property=围棋 → 张三; doc-scoped filters; limit caps results.
#[tokio::test]
async fn search_objects_filters_and_limits() {
    let store = fresh().await;
    let did = seed_doc(&store, "人物志").await;
    let p1 = seed_person(&store, did, "张三", json!({"偏好": "围棋"})).await;
    let p2 = seed_person(&store, did, "李四", json!({"偏好": "象棋"})).await;
    // A place-typed object, so type filtering is exercised meaningfully.
    store
        .create_object(&KnowledgeObject {
            id: 0,
            doc_id: did,
            object_type: ObjectType::Place,
            name: "江南".into(),
            properties: json!({}),
            confidence: 0.7,
            created_at: 0,
        })
        .await
        .expect("create place");

    // By type: only the two persons.
    let persons = store
        .search_objects(None, Some("person"), None, None, 20)
        .await
        .expect("search by type");
    let person_names: Vec<&str> = persons.iter().map(|o| o.name.as_str()).collect();
    assert!(person_names.contains(&"张三") && person_names.contains(&"李四"));
    assert!(!person_names.contains(&"江南"), "place excluded");

    // By name substring.
    let zhang = store
        .search_objects(Some("张"), None, None, None, 20)
        .await
        .expect("search by name");
    assert_eq!(zhang.len(), 1, "one name match");
    assert_eq!(zhang[0].name, "张三");

    // By property value.
    let go = store
        .search_objects(None, None, Some("围棋"), None, 20)
        .await
        .expect("search by property");
    assert_eq!(go.len(), 1, "one property match");
    assert_eq!(go[0].id, p1, "property match is the right object");

    // By document scope.
    let scoped = store
        .search_objects(None, None, None, Some(did), 20)
        .await
        .expect("search by doc");
    assert_eq!(scoped.len(), 3, "all three objects in the document");

    // Unknown doc scope → empty.
    let empty = store
        .search_objects(None, None, None, Some(999_999), 20)
        .await
        .expect("search unknown doc");
    assert!(empty.is_empty(), "unknown doc → no results");

    // Limit.
    let limited = store
        .search_objects(None, None, None, None, 1)
        .await
        .expect("search with limit");
    assert_eq!(limited.len(), 1, "limit honored");

    // No filters → everything, ordered by id (p2 present).
    let all = store
        .search_objects(None, None, None, None, 20)
        .await
        .expect("search all");
    assert!(all.iter().any(|o| o.id == p2), "unfiltered returns all");
}

/// Objective: Verify `search_objects` escapes LIKE wildcards in the
/// query — a `%`/`_` in the name must match literally, not act as a
/// wildcard matching everything (previously `%` returned the whole table).
/// Invariants: query "%" → only the object whose name literally contains
/// %; query "_" behaves likewise.
#[tokio::test]
async fn search_objects_escapes_like_wildcards() {
    let store = fresh().await;
    let did = seed_doc(&store, "特殊名").await;
    let percent = seed_person(&store, did, "100%完成", json!({})).await;
    let underscore = seed_person(&store, did, "a_b", json!({})).await;
    let normal = seed_person(&store, did, "张三", json!({})).await;
    let _ = (percent, underscore, normal);

    // `%` must NOT match every row — only the literal-% name.
    let pct = store
        .search_objects(Some("%"), None, None, None, 20)
        .await
        .expect("search percent");
    assert_eq!(pct.len(), 1, "`%` must be literal, got {} rows", pct.len());
    assert_eq!(pct[0].name, "100%完成");

    // `_` must NOT act as the single-char wildcard matching 张三/a_b.
    let us = store
        .search_objects(Some("a_b"), None, None, None, 20)
        .await
        .expect("search underscore");
    assert_eq!(us.len(), 1, "`_` must be literal, got {} rows", us.len());
    assert_eq!(us[0].name, "a_b");
}

/// Objective: Verify `graph_counts` reports accurate row counts.
/// Invariants: after seeding one doc + three objects, counts match;
/// an empty fresh store reports all-zero.
#[tokio::test]
async fn graph_counts_matches_seeded_rows() {
    let empty = fresh().await;
    let e0 = empty.graph_counts().await.expect("empty counts");
    assert_eq!(e0.documents, 0, "no documents yet");
    assert_eq!(e0.objects, 0, "no objects yet");
    assert_eq!(e0.edges, 0, "no edges yet");
    assert_eq!(e0.evidence, 0, "no evidence yet");

    let store = fresh().await;
    let did = seed_doc(&store, "人物志").await;
    seed_person(&store, did, "张三", json!({"偏好": "围棋"})).await;
    seed_person(&store, did, "李四", json!({"偏好": "象棋"})).await;
    store
        .create_object(&KnowledgeObject {
            id: 0,
            doc_id: did,
            object_type: ObjectType::Place,
            name: "江南".into(),
            properties: json!({}),
            confidence: 0.7,
            created_at: 0,
        })
        .await
        .expect("create place");

    let counts = store.graph_counts().await.expect("counts");
    assert_eq!(counts.documents, 1, "one document");
    assert_eq!(counts.objects, 3, "three objects");
    assert_eq!(counts.edges, 0, "no edges seeded");
    assert_eq!(counts.evidence, 0, "no evidence seeded");
}

/// Objective: Verify edge creation + `get_edges_touching` returns both
/// outgoing and incoming edges for an object.
/// Invariants: A↔B edge is returned when querying either endpoint.
#[tokio::test]
async fn edge_touches_both_endpoints() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let a = seed_person(&store, did, "刘备", json!({})).await;
    let b = seed_person(&store, did, "关羽", json!({})).await;
    store
        .create_edge(&KnowledgeEdge {
            id: 0,
            source_id: a,
            target_id: b,
            predicate: "结义".into(),
            properties: json!({}),
            origin: Origin::Observed,
            confidence: 0.9,
            valid_from: Some(1),
            valid_to: None,
            created_at: now_ts(),
        })
        .await
        .expect("create edge");

    let from_a = store.get_edges_touching(a).await.expect("edges a");
    let from_b = store.get_edges_touching(b).await.expect("edges b");
    assert_eq!(from_a.len(), 1, "source endpoint sees the edge");
    assert_eq!(from_b.len(), 1, "target endpoint also sees the edge");
    assert_eq!(from_a[0].predicate, "结义");
    assert_eq!(from_a[0].valid_from, Some(1));
}

/// Objective: Verify evidence linking is idempotent (UNIQUE constraint)
/// and that get_evidence_for returns linked rows.
/// Invariants: linking the same (source, evidence) twice does not error
/// and does not duplicate the row returned.
#[tokio::test]
async fn evidence_link_is_idempotent() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let cid = seed_chapter(&store, did, 1).await;
    let oid = seed_person(&store, did, "赵云", json!({})).await;
    let eid = store
        .create_evidence(&Evidence {
            id: 0,
            doc_id: did,
            chapter_id: cid,
            start_offset: Some(0),
            end_offset: Some(5),
            content: "赵云单骑救主".into(),
            created_at: now_ts(),
        })
        .await
        .expect("create evidence");

    store
        .link_evidence(EvidenceSourceType::Object, oid, eid)
        .await
        .expect("link once");
    // Second link must be a no-op, not an error.
    store
        .link_evidence(EvidenceSourceType::Object, oid, eid)
        .await
        .expect("link twice is idempotent");

    let evs = store
        .get_evidence_for(EvidenceSourceType::Object, oid)
        .await
        .expect("get evidence");
    assert_eq!(evs.len(), 1, "duplicate link must not duplicate rows");
    assert_eq!(evs[0].content, "赵云单骑救主");
}

/// Objective: Verify world events persist their source byte spans and that
/// re-upserting the same (title, timestamp, span) is idempotent while the
/// same title at a different span stays a separate event.
/// Invariants: offsets round-trip; second upsert returns the same id; a
/// different start_offset creates a new row; participant links are no-ops
/// on repeat.
#[tokio::test]
async fn world_event_offsets_round_trip_and_upsert_is_idempotent() {
    let store = fresh().await;
    let id1 = store
        .upsert_world_event(
            "刘备曰",
            "dialogue",
            Some(3),
            None,
            "刘备曰：进攻",
            0.5,
            Some(100),
            Some(120),
        )
        .await
        .expect("insert event");
    // Same identity → same id (re-compile must not duplicate).
    let id1_again = store
        .upsert_world_event(
            "刘备曰",
            "dialogue",
            Some(3),
            None,
            "刘备曰：进攻",
            0.5,
            Some(100),
            Some(120),
        )
        .await
        .expect("re-insert");
    assert_eq!(id1, id1_again, "identical (title, ts, span) reuses the row");

    // Same title at a different span is a distinct event.
    let id2 = store
        .upsert_world_event(
            "刘备曰",
            "dialogue",
            Some(3),
            None,
            "刘备曰：撤退",
            0.5,
            Some(500),
            Some(520),
        )
        .await
        .expect("insert second");
    assert_ne!(id1, id2, "different span must not merge events");

    let events = store.list_world_events().await.expect("list");
    assert_eq!(events.len(), 2, "two span-distinct events");
    let first = events.iter().find(|e| e.id == id1).expect("first event");
    assert_eq!(first.start_offset, Some(100), "start span persisted");
    assert_eq!(first.end_offset, Some(120), "end span persisted");
    assert_eq!(first.timestamp, Some(3), "chapter persisted");

    // Participant link is idempotent (UNIQUE event_id+entity_id).
    store
        .link_event_participant(id1, "刘备", "speaker")
        .await
        .expect("link once");
    store
        .link_event_participant(id1, "刘备", "speaker")
        .await
        .expect("link twice is a no-op");
    let world = store
        .find_world_entity("刘备")
        .await
        .expect("query")
        .expect("participant upserted into world_entities");
    assert_eq!(world.name, "刘备");
}

/// Objective: Verify world-state slots persist with their event anchor and
/// byte span, that a re-observation of the same (entity, slot, event) is
/// idempotent, and that a later chapter appends history (ADD-only).
/// Invariants: offsets round-trip; second upsert returns the same id and
/// does not grow the list; a different chapter adds a row; entity filter
/// scopes the result.
#[tokio::test]
async fn world_state_slots_round_trip_and_append_history() {
    let store = fresh().await;
    let event_id = store
        .upsert_world_event(
            "吕布 杀 董卓",
            "action",
            Some(3),
            None,
            "吕布杀董卓",
            0.6,
            Some(100),
            Some(112),
        )
        .await
        .expect("event");

    let id1 = store
        .upsert_world_state(
            "董卓",
            "status",
            "deceased",
            Some(3),
            Some(event_id),
            Some(100),
            Some(112),
            0.75,
        )
        .await
        .expect("state 1");
    // Same (entity, slot, event, chapter) → same row (re-compile no-op).
    let id1_again = store
        .upsert_world_state(
            "董卓",
            "status",
            "deceased",
            Some(3),
            Some(event_id),
            Some(100),
            Some(112),
            0.75,
        )
        .await
        .expect("state re-upsert");
    assert_eq!(id1, id1_again, "identical observation must reuse the row");

    let all = store.list_world_states(None).await.expect("list all");
    assert_eq!(all.len(), 1, "idempotent upsert must not grow history");
    assert_eq!(all[0].entity_name, "董卓");
    assert_eq!(all[0].slot, "status");
    assert_eq!(all[0].value, "deceased");
    assert_eq!(all[0].chapter, Some(3));
    assert_eq!(
        all[0].event_id,
        Some(event_id),
        "state anchors to its event"
    );
    assert_eq!(all[0].start_offset, Some(100), "span persisted");
    assert_eq!(all[0].end_offset, Some(112));

    // A later chapter (different event) appends history — ADD-only.
    let event2 = store
        .upsert_world_event(
            "华雄 斩 某",
            "action",
            Some(5),
            None,
            "华雄斩某",
            0.6,
            Some(200),
            Some(210),
        )
        .await
        .expect("event 2");
    store
        .upsert_world_state(
            "华雄",
            "status",
            "deceased",
            Some(5),
            Some(event2),
            Some(200),
            Some(210),
            0.7,
        )
        .await
        .expect("state 2");

    let all = store.list_world_states(None).await.expect("list all");
    assert_eq!(all.len(), 2, "a new event appends, never overwrites");
    // Ordered by (chapter, id): ch3 first, ch5 second.
    assert_eq!(all[0].chapter, Some(3));
    assert_eq!(all[1].chapter, Some(5));

    // Entity-name filter scopes the query.
    let one = store.list_world_states(Some("董卓")).await.expect("filter");
    assert_eq!(one.len(), 1, "filter by entity name");
    assert_eq!(one[0].entity_name, "董卓");
}

mod views;
