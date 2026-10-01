//! Integration tests for the export / import bundle round trip.
//!
//! They drive `export_store` / `import_bundle` through the public API
//! (`plan/rules/rules.md` §4.2) and live here instead of inside
//! `memory_export.rs`, so that file keeps its headroom under the
//! one-file-per-1000-lines rule (§1) while the cases are unchanged.

use mnemosyne::knowledge::KnowledgeStore;
use mnemosyne::knowledge::SQLiteKnowledgeStore;
use mnemosyne::knowledge::memory_export::{
    EXPORT_FORMAT, EXPORT_VERSION, ExportBundle, ExportDocument, ExportEdge, ExportEvidence,
    ExportEvidenceLink, ExportMention, ExportObject, ExportWorldEntity, ExportWorldProfile,
    export_store, import_bundle,
};

/// Current unix timestamp, as the bundle's own `exported_at` convention.
fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn sample_bundle() -> ExportBundle {
    ExportBundle {
        format: EXPORT_FORMAT.to_string(),
        version: EXPORT_VERSION,
        exported_at: now_ts(),
        documents: vec![ExportDocument {
            title: "会话-导出".into(),
            author: None,
            doc_type: Some("dialog".into()),
            source: String::new(),
        }],
        objects: vec![ExportObject {
            doc_title: "会话-导出".into(),
            object_type: "person".into(),
            name: "用户".into(),
            properties: serde_json::json!({"偏好": "简洁"}),
            confidence: 0.8,
        }],
        edges: vec![ExportEdge {
            doc_title: "会话-导出".into(),
            source_name: "用户".into(),
            predicate: "偏好".into(),
            target_name: "简洁".into(),
            properties: serde_json::json!({}),
            origin: "observed".into(),
            confidence: 0.7,
            valid_from: None,
            valid_to: None,
        }],
        evidence: vec![ExportEvidence {
            doc_title: "会话-导出".into(),
            content: "我偏好简洁的架构。".into(),
            start_offset: Some(0),
            end_offset: Some(10),
        }],
        evidence_links: vec![ExportEvidenceLink {
            source_type: "object".into(),
            source_name: "用户".into(),
            edge: None,
            evidence_content: "我偏好简洁的架构。".into(),
            evidence_start: Some(0),
            evidence_end: Some(10),
        }],
        mentions: vec![],
        world_entities: vec![ExportWorldEntity {
            name: "用户".into(),
            entity_type: "person".into(),
            importance: 0.5,
        }],
        world_profiles: vec![ExportWorldProfile {
            entity_name: "用户".into(),
            key: "偏好".into(),
            value: "简洁".into(),
            confidence: 0.8,
            evidence_content: None,
            evidence_start: None,
            evidence_end: None,
        }],
        world_relations: vec![],
        world_events: vec![],
        world_states: vec![],
    }
}

/// Objective: Verify `export_store` round-trips — importing an exported
/// bundle into a fresh store reproduces the same graph.
/// Invariants: import returns created counts; the entity is queryable by
/// name; re-import is a no-op (objects_created == 0).
#[tokio::test]
async fn export_import_round_trip() {
    let src = SQLiteKnowledgeStore::open_in_memory()
        .await
        .expect("src store");
    let bundle = import_bundle(&src, &sample_bundle()).await.expect("seed");
    assert_eq!(bundle.documents_created, 1, "document created");
    assert_eq!(bundle.objects_created, 1, "object created");

    // Now export what we just imported.
    let exported = export_store(&src).await.expect("export");
    assert!(!exported.documents.is_empty(), "documents exported");
    assert!(!exported.objects.is_empty(), "objects exported");

    // Import into a fresh store and verify content is reproducible.
    let dst = SQLiteKnowledgeStore::open_in_memory()
        .await
        .expect("dst store");
    let stats = import_bundle(&dst, &exported).await.expect("re-import");
    assert_eq!(stats.objects_created, 1, "object recreated in fresh store");

    let obj = dst
        .find_object_by_name("用户", None)
        .await
        .expect("query")
        .expect("user entity present");
    assert_eq!(obj.properties["偏好"], "简洁", "properties preserved");

    // Re-import into the same store must be a no-op for objects.
    let again = import_bundle(&dst, &exported).await.expect("import again");
    assert_eq!(again.objects_created, 0, "idempotent: no new objects");
    assert_eq!(again.objects_merged, 1, "existing object merged instead");
}

/// Objective: Verify an unsupported format is rejected on import.
/// Invariants: wrong format → error mentioning the format.
#[tokio::test]
async fn import_rejects_unknown_format() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let mut bundle = sample_bundle();
    bundle.format = "other-tool".into();
    let err = import_bundle(&store, &bundle)
        .await
        .expect_err("must reject");
    assert!(
        err.to_string().contains("unsupported export format"),
        "clear error, got: {err}"
    );
}

/// Objective: Verify a bundle from a FUTURE format version is rejected —
/// silently importing v2-as-v1 would drop fields this build cannot read.
/// Invariants: version > EXPORT_VERSION → error naming the version;
/// version == EXPORT_VERSION still imports (covered by round-trip test).
#[tokio::test]
async fn import_rejects_newer_bundle_version() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let mut bundle = sample_bundle();
    bundle.version = EXPORT_VERSION + 1;
    let err = import_bundle(&store, &bundle)
        .await
        .expect_err("must reject");
    assert!(
        err.to_string().contains("version"),
        "error must name the version mismatch, got: {err}"
    );
}

/// Objective: Verify exporting an empty store yields an empty bundle, not
/// an error.
/// Invariants: empty store → all sections empty.
#[tokio::test]
async fn export_empty_store_is_empty() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let bundle = export_store(&store).await.expect("export");
    assert!(
        bundle.documents.is_empty(),
        "an empty bundle must export no documents"
    );
    assert!(
        bundle.objects.is_empty(),
        "an empty bundle must export no objects"
    );
    assert!(
        bundle.edges.is_empty(),
        "an empty bundle must export no edges"
    );
    assert!(
        bundle.evidence.is_empty(),
        "an empty bundle must export no evidence"
    );
    assert!(
        bundle.evidence_links.is_empty(),
        "an empty bundle must export no evidence links"
    );
}

/// Objective: Verify a world profile's evidence anchor exports its byte
/// SPAN (not just content), so a restore can re-attach the claim to the
/// right occurrence when the same sentence text appears at two offsets.
/// Invariants: evidence_start/end equal the source row's span; a profile
/// with no evidence keeps all three fields None.
#[tokio::test]
async fn profile_export_carries_evidence_span() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let did = store
        .create_document(&mnemosyne::knowledge::Document {
            id: 0,
            title: "span-doc".into(),
            author: None,
            doc_type: Some("text".into()),
            source: String::new(),
            created_at: now_ts(),
        })
        .await
        .expect("doc");
    let cid = store
        .create_chapter(&mnemosyne::knowledge::Chapter {
            id: 0,
            doc_id: did,
            chapter_no: 1,
            title: None,
            content: String::new(),
            start_offset: None,
            end_offset: None,
        })
        .await
        .expect("chapter");
    // Same sentence text at two different offsets (repeated paragraph).
    store
        .create_evidence(&mnemosyne::knowledge::Evidence {
            id: 0,
            doc_id: did,
            chapter_id: cid,
            start_offset: Some(10),
            end_offset: Some(20),
            content: "重复的句子。".into(),
            created_at: now_ts(),
        })
        .await
        .expect("ev a");
    let ev_b = store
        .create_evidence(&mnemosyne::knowledge::Evidence {
            id: 0,
            doc_id: did,
            chapter_id: cid,
            start_offset: Some(300),
            end_offset: Some(310),
            content: "重复的句子。".into(),
            created_at: now_ts(),
        })
        .await
        .expect("ev b");
    let eid = store
        .upsert_world_entity("孔明", "person", 0.9)
        .await
        .expect("entity");
    // Anchor to the SECOND occurrence — content-only matching would pick
    // ev_a (first scan hit) and silently move the claim.
    store
        .upsert_world_profile(eid, "status", "出师", 0.8, Some(ev_b))
        .await
        .expect("profile");

    let bundle = export_store(&store).await.expect("export");
    let p = bundle
        .world_profiles
        .iter()
        .find(|p| p.key == "status")
        .expect("profile exported");
    assert_eq!(
        p.evidence_start,
        Some(300),
        "export must carry the anchored span, got {:?}",
        p.evidence_start
    );
    assert_eq!(
        p.evidence_end,
        Some(310),
        "the mention span must round-trip its end offset"
    );
    assert_eq!(
        p.evidence_content.as_deref(),
        Some("重复的句子。"),
        "content still exported"
    );

    // Import into a fresh store: the claim must re-attach to the row at
    // the SAME span (300..310), not to the first occurrence (10..20).
    let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
    import_bundle(&dst, &bundle).await.expect("import");
    let profiles = dst.list_world_profiles().await.expect("list");
    let restored = profiles
        .iter()
        .find(|p| p.key == "status")
        .expect("restored profile");
    let evidence_id = restored
        .evidence_id
        .expect("restored profile keeps its anchor");
    // Locate the restored evidence row and check its span.
    let mut found_span = None;
    for doc in dst.list_documents().await.expect("docs") {
        for ev in dst.list_evidence_by_document(doc.id).await.expect("ev") {
            if ev.id == evidence_id {
                found_span = Some((ev.start_offset, ev.end_offset));
            }
        }
    }
    assert_eq!(
        found_span,
        Some((Some(300), Some(310))),
        "restore must re-anchor to the span-matching row"
    );
}

/// Objective: Verify the import joins a caller-owned transaction instead of
/// committing its own work. The import used to run statement by statement with
/// no transaction at all, so a failure halfway left the graph half-restored
/// while the caller was told "import failed". Now the whole bundle is one unit,
/// and a caller that already owns a transaction keeps control of it.
/// Invariants: the imported rows exist inside the caller's transaction, the
/// caller's rollback removes all of them, and a standalone import leaves no
/// transaction open behind it.
#[tokio::test]
async fn import_joins_a_caller_transaction_instead_of_committing() {
    let src = SQLiteKnowledgeStore::open_in_memory().await.expect("src");
    import_bundle(&src, &sample_bundle()).await.expect("seed");
    let bundle = export_store(&src).await.expect("export");

    let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
    dst.begin_transaction().await.expect("begin");
    let stats = import_bundle(&dst, &bundle).await.expect("import");
    assert!(
        stats.documents_created >= 1,
        "the import must run inside the caller's transaction, got {stats:?}"
    );
    assert!(
        dst.in_transaction().await.expect("read state"),
        "the import must not close a transaction it does not own"
    );
    dst.rollback_transaction().await.expect("rollback");
    assert!(
        dst.list_documents().await.expect("docs").is_empty(),
        "the caller's rollback must undo the whole import"
    );

    import_bundle(&dst, &bundle)
        .await
        .expect("standalone import");
    assert!(
        !dst.in_transaction().await.expect("read state"),
        "a standalone import must commit and leave no transaction open"
    );
    assert_eq!(
        dst.list_documents().await.expect("docs").len(),
        1,
        "the standalone import is committed"
    );
}

/// A bundle whose evidence holds the SAME sentence at two spans, with the link
/// pointing at the second occurrence.
fn duplicated_sentence_bundle() -> ExportBundle {
    ExportBundle {
        format: EXPORT_FORMAT.to_string(),
        version: EXPORT_VERSION,
        exported_at: now_ts(),
        documents: vec![ExportDocument {
            title: "重复段落".into(),
            author: None,
            doc_type: Some("text".into()),
            source: String::new(),
        }],
        objects: vec![ExportObject {
            doc_title: "重复段落".into(),
            object_type: "person".into(),
            name: "用户".into(),
            properties: serde_json::json!({}),
            confidence: 0.8,
        }],
        edges: vec![],
        evidence: vec![
            ExportEvidence {
                doc_title: "重复段落".into(),
                content: "重复的句子。".into(),
                start_offset: Some(10),
                end_offset: Some(20),
            },
            ExportEvidence {
                doc_title: "重复段落".into(),
                content: "重复的句子。".into(),
                start_offset: Some(300),
                end_offset: Some(310),
            },
        ],
        evidence_links: vec![ExportEvidenceLink {
            source_type: "object".into(),
            source_name: "用户".into(),
            edge: None,
            evidence_content: "重复的句子。".into(),
            evidence_start: Some(300),
            evidence_end: Some(310),
        }],
        mentions: vec![],
        world_entities: vec![],
        world_profiles: vec![],
        world_relations: vec![],
        world_events: vec![],
        world_states: vec![],
    }
}

/// Objective: Verify an evidence LINK re-attaches to the span-matching row
/// instead of the first sentence with the same text. Links resolved their
/// target by content alone, so a repeated sentence silently moved the
/// justification of an object (or edge) to the wrong occurrence — the defect the
/// world-profile path had already fixed.
/// Invariants: both occurrences are imported; the restored link reports the
/// span it was exported with.
#[tokio::test]
async fn evidence_link_re_anchors_to_the_span_matching_row() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    import_bundle(&store, &duplicated_sentence_bundle())
        .await
        .expect("import");

    let exported = export_store(&store).await.expect("export");
    assert_eq!(
        exported.evidence.len(),
        2,
        "both occurrences of the sentence must be imported"
    );
    let link = exported
        .evidence_links
        .iter()
        .find(|link| link.source_name == "用户")
        .expect("the object link is exported");
    assert_eq!(
        link.evidence_start,
        Some(300),
        "the link must stay on its own occurrence, not the first identical text"
    );
    assert_eq!(
        link.evidence_end,
        Some(310),
        "the exported link must keep its evidence end offset"
    );
}

/// Objective: Verify mentions survive the bundle. They were not exported at all,
/// so a backup/restore silently dropped the entity index — the object said who,
/// and nothing could say where in the text it appeared.
/// Invariants: the mention round-trips with its span and alias, and re-importing
/// the exported bundle does not duplicate it.
#[tokio::test]
async fn mentions_survive_the_bundle_round_trip() {
    let mut bundle = duplicated_sentence_bundle();
    bundle.mentions = vec![ExportMention {
        doc_title: "重复段落".into(),
        object_name: "用户".into(),
        start_offset: Some(300),
        end_offset: Some(310),
        alias_used: Some("我".into()),
        confidence: 0.9,
    }];

    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let stats = import_bundle(&store, &bundle).await.expect("import");
    assert_eq!(stats.mentions_created, 1, "the mention is imported");

    let exported = export_store(&store).await.expect("export");
    assert_eq!(exported.mentions.len(), 1, "the mention is exported");
    assert_eq!(
        exported.mentions[0].object_name, "用户",
        "the exported mention must name its object"
    );
    assert_eq!(
        exported.mentions[0].start_offset,
        Some(300),
        "the span round-trips"
    );
    assert_eq!(
        exported.mentions[0].alias_used.as_deref(),
        Some("我"),
        "the exported mention must record the alias used"
    );

    let again = import_bundle(&store, &exported).await.expect("re-import");
    assert_eq!(
        again.mentions_created, 0,
        "a re-import must not duplicate the mention"
    );
    assert_eq!(
        export_store(&store).await.expect("export").mentions.len(),
        1,
        "exactly one mention row remains"
    );
}

/// Objective: Verify the import's conflict policy is explicit and stable: an
/// identity that already exists gets its NON-identity columns refreshed from the
/// bundle ("the bundle wins") rather than being skipped. This is what makes a
/// restore reproduce the bundle — and it is also why importing an older bundle
/// over a newer store moves values backwards, which a caller must know.
/// Invariants: the later bundle's importance replaces the stored one.
#[tokio::test]
async fn re_import_refreshes_existing_rows_from_the_bundle() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let first = sample_bundle();
    import_bundle(&store, &first).await.expect("first import");
    let seeded = store
        .find_world_entity("用户")
        .await
        .expect("query")
        .expect("the entity was imported");
    assert_eq!(seeded.importance, 0.5, "the bundle's value landed");

    // Same identity, different non-identity value.
    let mut updated = sample_bundle();
    updated.world_entities[0].importance = 0.9;
    import_bundle(&store, &updated)
        .await
        .expect("second import");

    let refreshed = store
        .find_world_entity("用户")
        .await
        .expect("query")
        .expect("the entity still exists");
    assert_eq!(
        refreshed.importance, 0.9,
        "the bundle wins over the stored value — the documented policy"
    );
    assert_eq!(
        refreshed.id, seeded.id,
        "the identity was reused, not duplicated"
    );
}

/// Objective: Verify a restore REPORTS what it could not resolve. Unresolved
/// references were dropped silently, so a caller could not tell a clean import
/// from a lossy one — "imported 3 links" said nothing about the 4 it skipped.
/// Invariants: an edge whose target the bundle does not carry is not created,
/// and is counted in `unresolved_references`.
#[tokio::test]
async fn unresolved_references_are_counted() {
    let mut bundle = duplicated_sentence_bundle();
    bundle.edges = vec![ExportEdge {
        doc_title: "重复段落".into(),
        source_name: "用户".into(),
        predicate: "认识".into(),
        target_name: "不存在的人".into(),
        properties: serde_json::json!({}),
        origin: "observed".into(),
        confidence: 0.5,
        valid_from: None,
        valid_to: None,
    }];

    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let stats = import_bundle(&store, &bundle).await.expect("import");
    assert_eq!(stats.edges_created, 0, "the dangling edge is not created");
    assert_eq!(
        stats.unresolved_references, 1,
        "the skipped reference must be counted, got {stats:?}"
    );
}
