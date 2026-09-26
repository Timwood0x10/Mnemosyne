//! Integration tests for the unified pipeline entry point (`compile_source`).
//!
//! They drive the pipeline the way an external consumer does — through the
//! public API only (`plan/rules/rules.md` §4.2) — and live here instead of in
//! `compiler/pipeline.rs` so that file stays under the one-file-per-1000-lines
//! rule (§1). Each case pins one behaviour of the persisted V7 general model:
//! documents / objects / edges / evidence, world entities, events with their
//! source byte spans, and character-state slots.

use mnemosyne::compiler::pipeline::compile_source;
use mnemosyne::knowledge::document_source::DialogSource;
use mnemosyne::knowledge::domain_profile::DomainProfile;
use mnemosyne::knowledge::store::{KnowledgeStore, SQLiteKnowledgeStore};
use mnemosyne::types::Message;

fn profile() -> DomainProfile {
    DomainProfile::load("conversation_cognition").expect("pack")
}

/// Objective: Verify a dialog source compiles into the general model
/// WITHOUT materializing a `person` object named after the session title
/// (a dialog title is a session id, not a person — the fixed modeling).
/// Invariants: stats.documents == 1; stats.objects == 0; evidence >= 1;
/// no object named after the session title exists.
#[tokio::test]
async fn dialog_compiles_into_general_model() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let source = DialogSource::new(
        "session-1",
        "export.json",
        vec![
            Message::new("user", "我喜欢 Rust，目标是把编译器做稳定。"),
            Message::new("assistant", "好的，我们一步一步来。"),
        ],
    );
    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert_eq!(stats.documents, 1, "one dialog → one document");
    assert_eq!(
        stats.objects, 0,
        "dialog title must NOT become a person entity, got {stats:?}"
    );
    assert!(stats.evidence >= 1, "hint sentences become evidence");

    // The session title must not exist as a knowledge object.
    assert!(
        store
            .find_object_by_name("session-1", None)
            .await
            .expect("query")
            .is_none(),
        "dialog title must not be materialized as an object"
    );
}

/// Objective: Verify a txt file compiles end-to-end (FileSource path) and
/// extracts the person names from the body — not just the filename.
/// Invariants: stats.documents == 1; the character 刘备 from the body is
/// discovered (the old behavior materialized the FILE stem as a fake
/// `person` object instead of the text's people).
#[tokio::test]
async fn txt_compiles_into_general_model() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let path = std::env::temp_dir().join(format!(
        "lorescope_pipeline_test_{}_{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::write(
        &path,
        "刘备说：我计划下周发布新版本。关羽答道：同意。刘备又说：那就按灰度方案来。关羽再道：好。",
    )
    .expect("write");
    let source = mnemosyne::knowledge::document_source::FileSource::new(&path);
    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert_eq!(stats.documents, 1);
    assert!(
        stats.objects >= 1,
        "txt file must yield the body's people, got {stats:?}"
    );
    assert!(
        store
            .find_object_by_name("刘备", None)
            .await
            .expect("query")
            .is_some(),
        "刘备 (from the body) must be extracted as an entity"
    );
    let _ = std::fs::remove_file(&path);
}

/// Objective: Verify a novel-titled text registers known characters from
/// the novel dictionary (NEW-C20) — a character who narrates but rarely
/// speaks must still land in the graph with its aliases.
/// Invariants: 赵云 exists with source=novel_dictionary and 子龙 among
/// aliases; the corpus-only path would miss him (no dialogue verb).
#[tokio::test]
async fn novel_dictionary_registers_known_cast() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let text = "赵云字子龙，常山真定人也。其人身长八尺，姿颜雄伟。";
    let source =
        mnemosyne::knowledge::document_source::RawTextSource::new("三国演义", "test", text, "text");
    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert!(stats.objects >= 1, "novel cast registered, got {stats:?}");

    let zhaoyun = store
        .find_object_by_name("赵云", None)
        .await
        .expect("query")
        .expect("赵云 must exist via novel dictionary");
    assert_eq!(
        zhaoyun.properties.get("source").and_then(|v| v.as_str()),
        Some("novel_dictionary"),
        "registered from the curated dictionary"
    );
    let aliases = zhaoyun
        .properties
        .get("aliases")
        .and_then(|a| a.as_array())
        .expect("aliases array");
    assert!(
        aliases.iter().any(|a| a.as_str() == Some("子龙")),
        "alias 子龙 must be attached, got {aliases:?}"
    );
    // The world model must also carry the entity.
    let we = store
        .find_world_entity("赵云")
        .await
        .expect("query")
        .expect("world entity");
    assert_eq!(we.name, "赵云");
}

/// Objective: Verify an empty source produces zero rows (no phantom
/// entities).
/// Invariants: empty dialog → stats all zero; no panic.
#[tokio::test]
async fn empty_source_yields_zero_stats() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let source = DialogSource::new("empty", "export.json", Vec::new());
    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert_eq!(stats.documents, 0);
    assert_eq!(stats.objects, 0);
    assert_eq!(stats.evidence, 0);
}

/// Objective: Verify entity upsert — compiling the same non-dialog text
/// title twice reuses the existing entity instead of creating a duplicate.
/// Invariants: first run objects == 1; second run objects == 0; exactly
/// one persisted object named after the title (the title is a real name,
/// so it legitimately anchors the document's entity).
#[tokio::test]
async fn recompile_same_title_reuses_entity() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let source = mnemosyne::knowledge::document_source::RawTextSource::new(
        "刘备",
        "export.json",
        "我喜欢 Rust，目标是稳定可靠。",
        "text",
    );

    let first = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("first compile");
    assert_eq!(first.objects, 1, "first run creates the entity");

    let second = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("second compile");
    assert_eq!(second.objects, 0, "second run reuses, does not duplicate");

    let obj = store
        .find_object_by_name("刘备", None)
        .await
        .expect("query")
        .expect("entity still present");
    assert_eq!(obj.name, "刘备", "single entity persists");
}

/// Objective: Verify relation sentences do NOT spawn placeholder Concept
/// entities ("no phantom entities" rule) and are instead recorded on the
/// entity as structured `relations` records.
/// Invariants: one document → objects == 1 (main entity only), edges == 0,
/// and the entity's `relations` array is non-empty.
#[tokio::test]
async fn relation_sentences_do_not_spawn_concepts() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let source = mnemosyne::knowledge::document_source::RawTextSource::new(
        "曹操",
        "export.json",
        "我喜欢简洁架构，目标是长期稳定。",
        "text",
    );

    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert_eq!(stats.documents, 1, "one document → one document");
    assert_eq!(
        stats.objects, 1,
        "no phantom concept entities, got {stats:?}"
    );
    assert_eq!(stats.edges, 0, "no edges to placeholder targets");

    let obj = store
        .find_object_by_name("曹操", None)
        .await
        .expect("query")
        .expect("entity");
    let rels = obj
        .properties
        .get("relations")
        .and_then(|r| r.as_array())
        .expect("relations array present");
    assert!(!rels.is_empty(), "relation sentences recorded on entity");
}

/// Objective: Verify the general pipeline also lands in the V7 world model
/// (`world_entities` + `world_entity_profiles`), which were previously
/// left empty ("V7 integration").
/// Invariants: after compiling a text document whose title is a real
/// person name, a `world_entities` row exists named after it, with at
/// least one profile when hints were extracted. Non-name titles (session
/// ids, generated ids) are excluded by design.
#[tokio::test]
async fn compile_writes_v7_world_entities() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let source = mnemosyne::knowledge::document_source::RawTextSource::new(
        "刘备",
        "export.json",
        "我喜欢简洁架构，目标是长期稳定。",
        "text",
    );

    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert_eq!(stats.documents, 1, "one document compiled");

    let world = store
        .find_world_entity("刘备")
        .await
        .expect("query world entity")
        .expect("world_entities row must be written by compile");
    assert_eq!(world.entity_type, "person", "default entity type");
    assert_eq!(world.name, "刘备", "entity name matches the title");

    // Every profile claim must carry an evidence row with a non-empty
    // source byte span — content-only anchors are not re-locatable.
    let profiles = store.list_world_profiles().await.expect("list profiles");
    assert!(
        !profiles.is_empty(),
        "hint extraction must produce at least one world profile"
    );
    for p in &profiles {
        let evidence_id = p
            .evidence_id
            .unwrap_or_else(|| panic!("profile {} must carry evidence_id", p.key));
        let mut found = false;
        for doc in store.list_documents().await.expect("docs") {
            for ev in store.list_evidence_by_document(doc.id).await.expect("ev") {
                if ev.id == evidence_id {
                    found = true;
                    assert!(
                        ev.start_offset.is_some() && ev.end_offset.is_some(),
                        "profile {} evidence must carry byte offsets",
                        p.key
                    );
                    assert!(
                        ev.end_offset > ev.start_offset,
                        "profile {} evidence span must be non-empty",
                        p.key
                    );
                }
            }
        }
        assert!(found, "profile {} evidence_id must resolve", p.key);
    }
}

/// Objective: Verify Pass2 story events persist to the V7 `events` table
/// with non-empty source byte spans (the production write path for
/// middle-IR Event offsets).
/// Invariants: prose with a discovered cast produces >= 1 world event;
/// every event carries start/end offsets with start < end; a second
/// compile of the same source does not duplicate events.
#[tokio::test]
async fn prose_persists_world_events_with_byte_spans() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let source = mnemosyne::knowledge::document_source::RawTextSource::new(
        "刘备",
        "export.json",
        "刘备曰：今日起兵。关羽道：愿随兄长。刘备救关羽于阵前。",
        "text",
    );

    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert!(
        stats.events >= 1,
        "Pass2 must persist at least one world event, got {stats:?}"
    );

    let events = store.list_world_events().await.expect("list events");
    assert!(
        !events.is_empty(),
        "events table must hold the compiled events"
    );
    for ev in &events {
        let start = ev
            .start_offset
            .unwrap_or_else(|| panic!("event `{}` must carry start_offset", ev.title));
        let end = ev
            .end_offset
            .unwrap_or_else(|| panic!("event `{}` must carry end_offset", ev.title));
        assert!(
            start < end,
            "event `{}` span must be non-empty, got {start}..{end}",
            ev.title
        );
    }

    // Re-compile: identical (title, timestamp, span) must not duplicate.
    let before = store.list_world_events().await.expect("list before").len();
    let second = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("recompile");
    let after = store.list_world_events().await.expect("list after").len();
    assert_eq!(before, after, "re-compile must not duplicate world events");
    assert!(second.events <= before, "second run reuses existing rows");
}

/// Objective: Verify the production write path for character-state slots
/// — Pass2 kill events extract `status=deceased` for the victim and
/// persist it to `world_states` with the event anchor + byte span.
/// Invariants: prose with a novel cast yields >= 1 state row; the victim
/// has slot=status/value=deceased; every row carries event_id and a
/// non-empty span; a second compile does not duplicate rows.
#[tokio::test]
async fn prose_persists_world_state_slots() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    // Novel title → NovelProvider seeds 吕布/董卓 into the cast, so the
    // Pass2 dict can resolve both participants of the kill.
    let source = mnemosyne::knowledge::document_source::RawTextSource::new(
        "三国演义",
        "export.json",
        "吕布杀董卓。",
        "text",
    );

    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert!(
        stats.states >= 1,
        "kill event must yield a state slot, got {stats:?}"
    );

    let states = store.list_world_states(None).await.expect("list");
    assert!(!states.is_empty(), "world_states must hold the slot");
    let victim = states
        .iter()
        .find(|s| s.slot == "status" && s.value == "deceased")
        .unwrap_or_else(|| panic!("a deceased status slot must exist, got {states:?}"));
    assert_eq!(
        victim.entity_name, "董卓",
        "kill marks the OBJECT, not the killer"
    );
    assert!(
        victim.event_id.is_some(),
        "state row must anchor to its source event"
    );
    let start = victim
        .start_offset
        .unwrap_or_else(|| panic!("state must carry start_offset"));
    let end = victim
        .end_offset
        .unwrap_or_else(|| panic!("state must carry end_offset"));
    assert!(start < end, "state span must be non-empty: {start}..{end}");

    // Re-compile: same (entity, slot, event, chapter) is a no-op.
    let before = store.list_world_states(None).await.expect("before").len();
    compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("recompile");
    let after = store.list_world_states(None).await.expect("after").len();
    assert_eq!(before, after, "re-compile must not duplicate state rows");
}

/// Objective: Verify dialog titles do NOT leak into the V7 world model —
/// a session id must not become a world entity (same modeling fix as the
/// object graph).
/// Invariants: compiling a dialog yields no `world_entities` row named
/// after the session title.
#[tokio::test]
async fn dialog_title_not_in_v7_world_entities() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let source = DialogSource::new(
        "session-no-v7",
        "export.json",
        vec![Message::new("user", "我喜欢 Rust，目标是稳定可靠。")],
    );

    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert_eq!(stats.objects, 0, "dialog → no objects");

    let world = store
        .find_world_entity("session-no-v7")
        .await
        .expect("query");
    assert!(
        world.is_none(),
        "dialog title must not become a world entity"
    );
}

/// Objective: Verify corpus entity discovery — a non-dialog text with
/// several speakers yields entities for the main speakers, not just the
/// doc-title anchor. This is the "entities come from the text, not a
/// dictionary" guarantee.
///
/// The heuristic is intentionally tolerant: prose like `大哥所言` may also
/// surface an honorary term, but the *data* always comes from the text,
/// and the main speakers are always present. We assert the core cast is
/// discovered (anchor + 刘备 + 关羽), not an exact object count.
/// Invariants: the three core entities exist and corpus ones are flagged;
/// every discovered corpus entity recurs >= 2 times (frequency gate).
#[tokio::test]
async fn corpus_text_discovers_multiple_entities() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let source = mnemosyne::knowledge::document_source::RawTextSource::new(
        "会谈纪要",
        "paste",
        "刘备说道：此事需从长计议。关羽道：大哥所言极是。刘备又说道：那便依计行事。关羽曰：此计可行。",
        "text",
    );

    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    // Anchor + the discovered cast: must exceed the single-anchor case.
    assert!(
        stats.objects >= 3,
        "corpus discovery must surface the speakers, got {stats:?}"
    );

    for name in ["会谈纪要", "刘备", "关羽"] {
        let obj = store
            .find_object_by_name(name, None)
            .await
            .expect("query")
            .unwrap_or_else(|| panic!("entity `{name}` must be discovered"));
        if name != "会谈纪要" {
            assert_eq!(
                obj.properties["discovered"], true,
                "corpus entities are flagged as discovered"
            );
            let freq = obj.properties["frequency"]
                .as_str()
                .unwrap_or_default()
                .parse::<usize>()
                .unwrap_or(0);
            assert!(
                freq >= 2,
                "corpus entity `{name}` must recur >=2 times, got freq={freq}"
            );
        }
    }
}

/// Objective: Verify English prose through the general pipeline persists
/// world events AND a character-state slot — `Config::default()` was
/// Chinese-only (English yielded zero action events), positional title
/// parsing broke on multi-word names, and the ASCII guard must accept
/// "died"/"killed" while rejecting progressive "killing".
///
/// Uses a single-token English name: multi-word English cast discovery
/// (title-person split into given/family tokens) is pre-existing pipeline
/// scope, untouched by this campaign.
///
/// Invariants: stats.events >= 2; stats.states >= 1; the deceased slot
/// anchors to the English subject with an event id + non-empty span.
#[tokio::test]
async fn english_prose_persists_events_and_death_state() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let source = mnemosyne::knowledge::document_source::RawTextSource::new(
        "Corvin",
        "export.json",
        "Corvin killed the bandit in the hallway. Corvin died at dawn.",
        "text",
    );

    let stats = compile_source(&source, &profile(), &store, "t1")
        .await
        .expect("compile");
    assert!(
        stats.events >= 2,
        "English prose must yield kill + death events, got {stats:?}"
    );
    assert!(
        stats.states >= 1,
        "English death must yield a state slot, got {stats:?}"
    );

    let states = store.list_world_states(None).await.expect("list");
    let victim = states
        .iter()
        .find(|s| s.slot == "status" && s.value == "deceased")
        .unwrap_or_else(|| panic!("a deceased status slot must exist, got {states:?}"));
    assert_eq!(
        victim.entity_name, "Corvin",
        "intransitive death marks the subject"
    );
    assert!(
        victim.event_id.is_some(),
        "state row must anchor to its source event"
    );
    let start = victim
        .start_offset
        .unwrap_or_else(|| panic!("state must carry start_offset"));
    let end = victim
        .end_offset
        .unwrap_or_else(|| panic!("state must carry end_offset"));
    assert!(start < end, "state span must be non-empty: {start}..{end}");
}

/// Objective: Verify two DIFFERENT sources sharing one title no longer
/// merge into a single doc_id (the documented title-only identity
/// limitation, fixed by the `documents.source` column). Invariants: two
/// compiles of the same title from different sources → 2 documents; a
/// re-compile of the same source reuses its row (still 2).
#[tokio::test]
async fn same_title_from_different_sources_stays_separate() {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let mk = |source: &'static str, text: &'static str| {
        mnemosyne::knowledge::document_source::RawTextSource::new(
            "shared-title",
            source,
            text,
            "text",
        )
    };

    let first = compile_source(
        &mk("source-a", "甲说：开始推进。"),
        &profile(),
        &store,
        "t1",
    )
    .await
    .expect("compile a");
    assert_eq!(first.documents, 1, "first source creates its document");

    let second = compile_source(
        &mk("source-b", "乙说：开始推进。"),
        &profile(),
        &store,
        "t1",
    )
    .await
    .expect("compile b");
    assert_eq!(
        second.documents, 1,
        "second source creates its own document"
    );

    let docs = store.list_documents().await.expect("list");
    assert_eq!(
        docs.iter().filter(|d| d.title == "shared-title").count(),
        2,
        "same title from two sources must be two rows, got {:?}",
        docs.iter()
            .map(|d| (&d.title, &d.source))
            .collect::<Vec<_>>()
    );

    // Same source again → reuse, not a third row.
    compile_source(
        &mk("source-a", "甲说：开始推进。"),
        &profile(),
        &store,
        "t1",
    )
    .await
    .expect("recompile a");
    let docs = store.list_documents().await.expect("list after");
    assert_eq!(
        docs.iter().filter(|d| d.title == "shared-title").count(),
        2,
        "re-compiling an existing (title, source) must reuse its row"
    );
}
