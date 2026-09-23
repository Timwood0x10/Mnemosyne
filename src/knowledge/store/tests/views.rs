//! Knowledge-store unit tests for the query projections.

use super::*;

/// Objective: Verify `inspect_entity` returns Object + Edges + Evidence
/// and splits participated_in edges into the events list.
/// Invariants: a person with one relation + one participated_in event +
/// one evidence yields 1 relation, 1 event, 1 evidence.
#[tokio::test]
async fn inspect_entity_returns_full_picture() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let zhaoyun = seed_person(&store, did, "赵云", json!({})).await;
    let liubei = seed_person(&store, did, "刘备", json!({})).await;
    let event_id = store
        .create_object(&KnowledgeObject {
            id: 0,
            doc_id: did,
            object_type: ObjectType::Event,
            name: "单骑救主".into(),
            properties: json!({}),
            confidence: 1.0,
            created_at: now_ts(),
        })
        .await
        .expect("create event object");

    let cid = seed_chapter(&store, did, 41).await;
    let eid = store
        .create_evidence(&Evidence {
            id: 0,
            doc_id: did,
            chapter_id: cid,
            start_offset: None,
            end_offset: None,
            content: "赵云怀抱阿斗，杀透重围".into(),
            created_at: now_ts(),
        })
        .await
        .expect("create evidence");

    let trust_edge = KnowledgeEdge {
        id: 0,
        source_id: liubei,
        target_id: zhaoyun,
        predicate: "trusts".into(),
        properties: json!({}),
        origin: Origin::Observed,
        confidence: 0.7,
        valid_from: Some(41),
        valid_to: None,
        created_at: now_ts(),
    };
    store
        .create_edge(&trust_edge)
        .await
        .expect("create trust edge");
    let part_edge = KnowledgeEdge {
        id: 0,
        source_id: zhaoyun,
        target_id: event_id,
        predicate: "participated_in".into(),
        properties: json!({}),
        origin: Origin::Observed,
        confidence: 1.0,
        valid_from: Some(41),
        valid_to: None,
        created_at: now_ts(),
    };
    let part_edge_id = store
        .create_edge(&part_edge)
        .await
        .expect("create part edge");
    store
        .link_evidence(EvidenceSourceType::Object, zhaoyun, eid)
        .await
        .expect("link object evidence");
    store
        .link_evidence(EvidenceSourceType::Edge, part_edge_id, eid)
        .await
        .expect("link edge evidence");

    let result = store
        .inspect_entity("赵云", Some("三国演义"))
        .await
        .expect("inspect")
        .expect("entity found");
    assert_eq!(result.object.name, "赵云");
    assert_eq!(result.relations.len(), 1, "one person↔person relation");
    assert_eq!(result.events.len(), 1, "one participated_in event");
    assert_eq!(result.events[0].name, "单骑救主");
    assert_eq!(
        result.evidences.len(),
        1,
        "evidence deduped across object+edge links"
    );
    assert!(result.evidences.iter().any(|e| e.content.contains("阿斗")));
}

/// Objective: Verify `inspect_entity` returns None for an unknown entity
/// rather than erroring.
/// Invariants: unknown name → Ok(None); unknown doc title → Err.
#[tokio::test]
async fn inspect_entity_missing() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    seed_person(&store, did, "赵云", json!({})).await;
    assert!(
        store
            .inspect_entity("不存在", Some("三国演义"))
            .await
            .expect("unknown entity")
            .is_none()
    );
    let err = store
        .inspect_entity("赵云", Some("不存在的书"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::NotFound(_)),
        "unknown doc should be NotFound"
    );
}

/// Objective: Verify `get_objects_bulk` returns every requested object in
/// one call, silently skipping ids that do not exist (mirrors a per-id
/// `get_object` loop without the N queries).
/// Invariants: 2 of 3 requested ids exist → 2 objects returned; empty
/// input → empty vec.
#[tokio::test]
async fn get_objects_bulk_fetches_many() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let zhaoyun = seed_person(&store, did, "赵云", json!({})).await;
    let liubei = seed_person(&store, did, "刘备", json!({})).await;

    let objs = store
        .get_objects_bulk(&[zhaoyun, liubei, 99_999])
        .await
        .expect("bulk fetch");
    let names: Vec<&str> = objs.iter().map(|o| o.name.as_str()).collect();
    assert!(
        names.contains(&"赵云") && names.contains(&"刘备"),
        "both existing objects returned, got {names:?}"
    );
    assert_eq!(
        objs.len(),
        2,
        "missing id 99999 must be skipped, got {names:?}"
    );

    let empty = store.get_objects_bulk(&[]).await.expect("empty bulk");
    assert!(empty.is_empty(), "empty input → empty output");
}

/// Objective: Verify `get_evidence_for_many` returns evidence linked to
/// ANY of the given edge ids in one call (the inspect_entity N+1 fix).
/// Invariants: two edges sharing evidence → both edge links are returned
/// (the bulk method reports per-edge links; cross-edge dedup happens in
/// inspect_entity); empty input → empty vec.
#[tokio::test]
async fn get_evidence_for_many_fetches_across_edges() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let zhaoyun = seed_person(&store, did, "赵云", json!({})).await;
    let liubei = seed_person(&store, did, "刘备", json!({})).await;
    let cid = seed_chapter(&store, did, 41).await;

    let eid = store
        .create_evidence(&Evidence {
            id: 0,
            doc_id: did,
            chapter_id: cid,
            start_offset: None,
            end_offset: None,
            content: "赵云怀抱阿斗".into(),
            created_at: now_ts(),
        })
        .await
        .expect("create evidence");

    // Two edges touching the same evidence.
    let mut e1_id = 0i64;
    let mut e2_id = 0i64;
    for predicate in ["trusts", "protects"] {
        let edge_id = store
            .create_edge(&KnowledgeEdge {
                id: 0,
                source_id: zhaoyun,
                target_id: liubei,
                predicate: predicate.into(),
                properties: json!({}),
                origin: Origin::Observed,
                confidence: 0.8,
                valid_from: Some(41),
                valid_to: None,
                created_at: now_ts(),
            })
            .await
            .expect("create edge");
        store
            .link_evidence(EvidenceSourceType::Edge, edge_id, eid)
            .await
            .expect("link edge evidence");
        if e1_id == 0 {
            e1_id = edge_id;
        } else {
            e2_id = edge_id;
        }
    }

    let evs = store
        .get_evidence_for_many(EvidenceSourceType::Edge, &[e1_id, e2_id])
        .await
        .expect("bulk evidence");
    assert_eq!(
        evs.len(),
        2,
        "two edge links to the same evidence → two rows (dedup is inspect_entity's job)"
    );
    assert!(
        evs.iter().all(|e| e.content.contains("阿斗")),
        "evidence content preserved on every row"
    );

    let empty = store
        .get_evidence_for_many(EvidenceSourceType::Edge, &[])
        .await
        .expect("empty bulk");
    assert!(empty.is_empty(), "empty input → empty output");
}

/// Objective: Verify `entity_timeline` orders entries by chapter and
/// reports the predicate + target for each edge.
/// Invariants: a 2-edge entity yields 2 entries sorted ascending by chapter.
#[tokio::test]
async fn timeline_orders_by_chapter() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let lvbu = seed_person(&store, did, "吕布", json!({})).await;
    let dingyuan = seed_person(&store, did, "丁原", json!({})).await;

    // 吕布 kills 丁原 at ch3, serves 丁原 at ch1 — insert out of order.
    for (pred, ch) in [("kills", 3), ("serves", 1)] {
        store
            .create_edge(&KnowledgeEdge {
                id: 0,
                source_id: lvbu,
                target_id: dingyuan,
                predicate: pred.into(),
                properties: json!({}),
                origin: Origin::Observed,
                confidence: 0.8,
                valid_from: Some(ch),
                valid_to: None,
                created_at: now_ts(),
            })
            .await
            .expect("create edge");
    }
    let tl = store
        .entity_timeline("吕布", Some("三国演义"))
        .await
        .expect("timeline");
    assert_eq!(tl.len(), 2);
    assert_eq!(tl[0].chapter, Some(1), "serves@ch1 must come first");
    assert_eq!(tl[0].predicate, "serves");
    assert_eq!(tl[1].chapter, Some(3));
    assert_eq!(tl[1].predicate, "kills");
    assert_eq!(tl[0].target, "丁原");
}

/// Objective: Verify `relation_graph` BFS returns the root + 1-hop
/// neighbors and dedupes edges via petgraph.
/// Invariants: depth=1 around 刘备 (结义 关羽, 结义 张飞) yields 3 nodes
/// and 2 edges.
#[tokio::test]
async fn relation_graph_bfs_dedupes() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let liubei = seed_person(&store, did, "刘备", json!({})).await;
    let guanyu = seed_person(&store, did, "关羽", json!({})).await;
    let zhangfei = seed_person(&store, did, "张飞", json!({})).await;
    for tgt in [guanyu, zhangfei] {
        store
            .create_edge(&KnowledgeEdge {
                id: 0,
                source_id: liubei,
                target_id: tgt,
                predicate: "结义".into(),
                properties: json!({}),
                origin: Origin::Observed,
                confidence: 1.0,
                valid_from: Some(1),
                valid_to: None,
                created_at: now_ts(),
            })
            .await
            .expect("create edge");
    }
    let g = store
        .relation_graph("刘备", 1, Some("三国演义"))
        .await
        .expect("graph")
        .expect("root found");
    assert_eq!(g.nodes.len(), 3, "root + 2 brothers");
    let names: HashSet<String> = g.nodes.iter().map(|n| n.name.clone()).collect();
    assert!(names.contains("关羽"));
    assert!(names.contains("张飞"));
    assert_eq!(g.edges.len(), 2, "two 结义 edges");
}

/// Objective: Verify `relation_graph` returns exactly `depth` hops of
/// nodes — not `depth+1` (regression for the `0..=depth` off-by-one).
/// Invariants: a linear chain 刘备→关羽→曹操 with depth=1 yields {刘备, 关羽}
/// and one edge; node 曹操 (2 hops) must NOT appear.
#[tokio::test]
async fn relation_graph_depth_does_not_overreach() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let liubei = seed_person(&store, did, "刘备", json!({})).await;
    let guanyu = seed_person(&store, did, "关羽", json!({})).await;
    let caocao = seed_person(&store, did, "曹操", json!({})).await;
    // Chain: 刘备 → 关羽 → 曹操
    for (src, tgt) in [(liubei, guanyu), (guanyu, caocao)] {
        store
            .create_edge(&KnowledgeEdge {
                id: 0,
                source_id: src,
                target_id: tgt,
                predicate: "结义".into(),
                properties: json!({}),
                origin: Origin::Observed,
                confidence: 1.0,
                valid_from: Some(1),
                valid_to: None,
                created_at: now_ts(),
            })
            .await
            .expect("create edge");
    }
    let g = store
        .relation_graph("刘备", 1, Some("三国演义"))
        .await
        .expect("graph")
        .expect("root found");
    let names: HashSet<String> = g.nodes.iter().map(|n| n.name.clone()).collect();
    assert!(
        names.contains("刘备") && names.contains("关羽"),
        "depth=1 must include root + direct neighbor"
    );
    assert!(
        !names.contains("曹操"),
        "depth=1 must NOT include 2-hop node 曹操 (off-by-one regression)"
    );
    assert_eq!(g.edges.len(), 1, "depth=1 chain yields one edge");
}

/// Objective: Verify `relation_graph` preserves parallel edges between
/// the same pair (regression for DiGraphMap collapsing them to last-wins).
/// Invariants: two edges 刘备→关羽 ("结义" and "trusts") yield 2 edges with
/// both predicates present.
#[tokio::test]
async fn relation_graph_keeps_parallel_edges() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let liubei = seed_person(&store, did, "刘备", json!({})).await;
    let guanyu = seed_person(&store, did, "关羽", json!({})).await;
    for pred in ["结义", "trusts"] {
        store
            .create_edge(&KnowledgeEdge {
                id: 0,
                source_id: liubei,
                target_id: guanyu,
                predicate: pred.into(),
                properties: json!({}),
                origin: Origin::Observed,
                confidence: 0.9,
                valid_from: Some(1),
                valid_to: None,
                created_at: now_ts(),
            })
            .await
            .expect("create edge");
    }
    let g = store
        .relation_graph("刘备", 1, Some("三国演义"))
        .await
        .expect("graph")
        .expect("root found");
    let preds: HashSet<String> = g.edges.iter().map(|e| e.predicate.clone()).collect();
    assert!(
        preds.contains("结义") && preds.contains("trusts"),
        "parallel edges must both survive, got predicates {preds:?}"
    );
    assert_eq!(g.edges.len(), 2, "two distinct relations → two edges");
}

/// Objective: Verify `search_evidence` matches content by substring and
/// can be scoped to a document.
/// Invariants: a LIKE query returns matching evidence with chapter + doc.
#[tokio::test]
async fn search_evidence_matches_content() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let cid = seed_chapter(&store, did, 41).await;
    store
        .create_evidence(&Evidence {
            id: 0,
            doc_id: did,
            chapter_id: cid,
            start_offset: None,
            end_offset: None,
            content: "赵云单骑救阿斗".into(),
            created_at: now_ts(),
        })
        .await
        .expect("create evidence");
    let hits = store
        .search_evidence("阿斗", Some("三国演义"), 10)
        .await
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chapter, 41);
    assert_eq!(hits[0].doc, "三国演义");
    assert_eq!(hits[0].confidence, 1.0);
}

/// Objective: Verify a query containing a literal backslash (`C:\`) is
/// escaped before the LIKE pattern — previously the trailing `\` escaped
/// the closing `%`, so the search silently matched the wrong rows.
/// Invariants: a query with `\` finds the exact backslash content and
/// does not error; `%`/`_` remain literal (wildcard-free).
#[tokio::test]
async fn search_evidence_escapes_backslash() {
    let store = fresh().await;
    let did = seed_doc(&store, "日志").await;
    let cid = seed_chapter(&store, did, 1).await;
    for content in ["路径 C:\\data 在此", "普通文本无符号", "百分之五十 50%"] {
        store
            .create_evidence(&Evidence {
                id: 0,
                doc_id: did,
                chapter_id: cid,
                start_offset: None,
                end_offset: None,
                content: content.into(),
                created_at: now_ts(),
            })
            .await
            .expect("create evidence");
    }
    // Backslash query must match only the backslash row, not all rows.
    let hits = store
        .search_evidence("C:\\data", Some("日志"), 10)
        .await
        .expect("search with backslash");
    assert_eq!(hits.len(), 1, "backslash query must be exact");
    assert_eq!(hits[0].text, "路径 C:\\data 在此");
    // A bare `%` query must NOT act as a wildcard matching everything.
    let pct = store
        .search_evidence("%", Some("日志"), 10)
        .await
        .expect("search with percent");
    assert_eq!(pct.len(), 1, "`%` must be literal, not a wildcard");
    assert_eq!(pct[0].text, "百分之五十 50%");
}
