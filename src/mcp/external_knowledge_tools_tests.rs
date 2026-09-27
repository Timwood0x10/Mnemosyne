//! Unit tests for the external-knowledge MCP tools.
//!
//! Extracted from `external_knowledge_tools.rs` via `#[path]` so the module
//! source stays under the 1000-line limit (plan/rules/rules.md §1). The tests
//! reach the parent's private items through `use super::*`.

use super::*;
use crate::knowledge::EntityLinker;
use crate::knowledge::ExternalKnowledgeRegistry;
use std::sync::RwLock;

/// Objective: Verify search_json_db returns substring matches sorted by
/// descending score, truncated to limit, and that an empty query matches
/// everything (used by materialize-all).
/// Invariants: 3 rows → 2 matches for "alpha"; sorted by score desc; limit
/// truncates; empty query returns all.
#[test]
fn search_json_db_matches_substring_and_sorts() {
    let rows = vec![
        JsonDbRow {
            id: "low".into(),
            text: "alpha beta".into(),
            score: 0.3,
        },
        JsonDbRow {
            id: "high".into(),
            text: "alpha gamma".into(),
            score: 0.9,
        },
        JsonDbRow {
            id: "miss".into(),
            text: "delta".into(),
            score: 1.0,
        },
    ];
    let hits = search_json_db(&rows, "alpha", 10);
    assert_eq!(hits.len(), 2, "two rows match 'alpha'");
    assert_eq!(hits[0].id, "high", "higher score ranks first");
    assert_eq!(hits[1].id, "low");

    // Limit truncates after sorting.
    let top1 = search_json_db(&rows, "alpha", 1);
    assert_eq!(top1.len(), 1);
    assert_eq!(top1[0].id, "high");

    // Empty query matches all rows (materialize-all path).
    let all = search_json_db(&rows, "", 10);
    assert_eq!(all.len(), 3, "empty query returns every row");

    // limit=0 short-circuits.
    assert!(search_json_db(&rows, "alpha", 0).is_empty());
}

/// Unique temp path per call — fixed names race across parallel test
/// processes (and across tests that clean up the same file mid-run).
fn unique_temp(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{name}_{}_{}", std::process::id(), n))
}

/// Objective: Verify load_json_db accepts both a bare array and a
/// {"documents": [...]} wrapper, and rejects rows missing id/text.
/// Invariants: bare array loads 2 rows; wrapper loads 1; missing id errors.
#[test]
fn load_json_db_accepts_array_and_wrapper() {
    // Unique paths: fixed names race when this test runs alongside others
    // that clean up the same temp files.
    let bare = unique_temp("lorescope_test_bare.json");
    std::fs::write(
        &bare,
        r#"[{"id":"1","text":"one","score":0.4},{"id":"2","text":"two"}]"#,
    )
    .expect("write bare");
    let rows = load_json_db(bare.to_str().unwrap()).expect("bare loads");
    assert_eq!(rows.len(), 2, "bare array yields 2 rows");
    assert_eq!(rows[1].score, 0.5, "missing score defaults to 0.5");

    // Documents wrapper.
    let wrapped = unique_temp("lorescope_test_wrapped.json");
    std::fs::write(&wrapped, r#"{"documents":[{"id":"x","text":"hello"}]}"#)
        .expect("write wrapped");
    let rows = load_json_db(wrapped.to_str().unwrap()).expect("wrapped loads");
    assert_eq!(rows.len(), 1, "wrapper yields 1 row");

    // Missing id → error.
    let bad = unique_temp("lorescope_test_bad.json");
    std::fs::write(&bad, r#"[{"text":"no id"}]"#).expect("write bad");
    let err = load_json_db(bad.to_str().unwrap()).unwrap_err();
    assert!(
        matches!(err, Error::InvalidInput(_)),
        "missing id must error, got {err:?}"
    );

    // Clean up.
    let _ = std::fs::remove_file(&bare);
    let _ = std::fs::remove_file(&wrapped);
    let _ = std::fs::remove_file(&bad);
}

/// Objective: Verify knowledge_attach with a document file registers the
/// source with the registry and rebuilds the linker so entity links become
/// resolvable.
/// Invariants: registry len grows by 1; linker resolves the supplied
/// external_name to the canonical; response reports documents_loaded.
#[tokio::test]
async fn knowledge_attach_document_registers_and_rebuilds_linker() {
    let registry = Arc::new(ExternalKnowledgeRegistry::new());
    let linker: SharedEntityLinker = Arc::new(RwLock::new(EntityLinker::new()));
    // Write a temp text file to attach (unique path — fixed names race).
    let path = unique_temp("lorescope_attach_test.txt");
    std::fs::write(&path, "Alice met Bob at the park.").expect("write txt");

    let handler = KnowledgeAttachHandler {
        registry: registry.clone(),
        linker: linker.clone(),
    };
    let args = serde_json::json!({
        "source_type": "document",
        "path": path.to_string_lossy(),
        "source_name": "story",
        "entity_links": [
            {"external_name": "Alicia", "canonical_name": "Alice", "source": "story"}
        ]
    });
    let result = handler.call(&args).await.expect("attach succeeds");
    assert!(!result.is_error, "attach must not error");
    assert_eq!(registry.len(), 1, "one adapter registered");
    assert_eq!(
        registry.source_names(),
        vec!["story".to_string()],
        "source name recorded"
    );
    // Linker rebuilt: "Alicia" resolves to "Alice".
    let guard = linker.read().expect("linker read");
    assert_eq!(
        guard.resolve("Alicia", Some("story")),
        Some("Alice"),
        "entity link is resolvable after attach"
    );
    drop(guard);

    // Verify the response payload.
    let payload: Value = serde_json::from_str(&result.content[0].text.clone().unwrap_or_default())
        .expect("response is JSON");
    assert_eq!(payload["documents_loaded"].as_u64(), Some(1));
    assert_eq!(payload["format"].as_str(), Some("text"));

    let _ = std::fs::remove_file(&path);
}

/// Objective: Verify knowledge_attach with a db file registers an index-mode
/// adapter that answers search_all (query-forwarded into hybrid retrieval).
/// Invariants: signal_provider_count grows to 1; search_all returns hits;
/// registry.kind_of reports Db.
#[tokio::test]
async fn knowledge_attach_db_registers_signal_provider() {
    let registry = Arc::new(ExternalKnowledgeRegistry::new());
    let linker: SharedEntityLinker = Arc::new(RwLock::new(EntityLinker::new()));
    let db_path = unique_temp("lorescope_attach_db.json");
    std::fs::write(
        &db_path,
        r#"[{"id":"r1","text":"rust async","score":0.8},{"id":"r2","text":"python sync"}]"#,
    )
    .expect("write db");

    let handler = KnowledgeAttachHandler {
        registry: registry.clone(),
        linker: linker.clone(),
    };
    let args = serde_json::json!({
        "source_type": "db",
        "connection": db_path.to_string_lossy(),
        "source_name": "tech-db"
    });
    let result = handler.call(&args).await.expect("attach succeeds");
    assert!(!result.is_error, "db attach must not error");
    assert_eq!(registry.signal_provider_count(), 1, "one signal provider");
    assert_eq!(
        registry.kind_of("tech-db"),
        Some(crate::knowledge::adapter::AdapterKind::Db),
        "kind is Db"
    );
    // search_all forwards to the JSON DB adapter.
    let hits = registry.search_all("rust", 5);
    assert_eq!(hits.len(), 1, "one row matches 'rust'");
    assert_eq!(hits[0].id, "r1");

    let _ = std::fs::remove_file(&db_path);
}

/// Objective: Verify knowledge_ingest materialize persists a document +
/// chapter + evidence into the knowledge store so the evidence tool can
/// find the content.
/// Invariants: documents_created >= 1; evidence search finds the body text.
#[tokio::test]
async fn knowledge_ingest_materializes_into_graph() {
    let registry = Arc::new(ExternalKnowledgeRegistry::new());
    let linker: SharedEntityLinker = Arc::new(RwLock::new(EntityLinker::new()));
    let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));

    // Attach a text document first.
    let path = unique_temp("lorescope_ingest_test.txt");
    std::fs::write(&path, "Zhaoyun charged through the enemy lines.").expect("write txt");
    let attach = KnowledgeAttachHandler {
        registry: registry.clone(),
        linker: linker.clone(),
    };
    attach
        .call(&serde_json::json!({
            "source_type": "document",
            "path": path.to_string_lossy(),
            "source_name": "ingest-src"
        }))
        .await
        .expect("attach");
    let _ = std::fs::remove_file(&path);

    // Materialize.
    let ingest = KnowledgeIngestHandler {
        registry: registry.clone(),
        store: store.clone(),
    };
    let result = ingest
        .call(&serde_json::json!({
            "source_name": "ingest-src",
            "mode": "materialize"
        }))
        .await
        .expect("ingest");
    assert!(!result.is_error, "ingest must not error");
    let payload: Value = serde_json::from_str(&result.content[0].text.clone().unwrap_or_default())
        .expect("ingest response is JSON");
    assert_eq!(payload["documents_created"].as_u64(), Some(1));
    assert_eq!(payload["evidence_created"].as_u64(), Some(1));

    // The evidence tool can now find the materialized content.
    let hits = store
        .search_evidence("Zhaoyun", None, 10)
        .await
        .expect("search");
    assert!(
        !hits.is_empty(),
        "materialized content is searchable via evidence tool"
    );
}

/// Objective: Verify knowledge_ingest is FULLY idempotent — a second
/// materialize of the same source must not duplicate chapters or evidence
/// (documents were already deduped; the fix extends dedup to the other two
/// row types).
/// Invariants: second call reports documents_created == 0, chapters_created
/// == 0, evidence_created == 0; the store holds exactly one chapter and one
/// evidence row for the document.
#[tokio::test]
async fn knowledge_ingest_reingest_is_fully_idempotent() {
    let registry = Arc::new(ExternalKnowledgeRegistry::new());
    let linker: SharedEntityLinker = Arc::new(RwLock::new(EntityLinker::new()));
    let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));

    // Attach a text document first.
    let path = unique_temp("lorescope_reingest_test.txt");
    std::fs::write(&path, "Liu Bei met Guan Yu in the peach garden.").expect("write txt");
    let attach = KnowledgeAttachHandler {
        registry: registry.clone(),
        linker: linker.clone(),
    };
    attach
        .call(&serde_json::json!({
            "source_type": "document",
            "path": path.to_string_lossy(),
            "source_name": "reingest-src"
        }))
        .await
        .expect("attach");
    let _ = std::fs::remove_file(&path);

    let ingest = KnowledgeIngestHandler {
        registry: registry.clone(),
        store: store.clone(),
    };
    let args = serde_json::json!({
        "source_name": "reingest-src",
        "mode": "materialize"
    });

    // First ingest: creates doc + chapter + evidence.
    let first = ingest.call(&args).await.expect("first ingest");
    assert!(!first.is_error, "first ingest must not error");
    let first_payload: Value =
        serde_json::from_str(&first.content[0].text.clone().unwrap_or_default())
            .expect("first response JSON");
    assert_eq!(first_payload["documents_created"].as_u64(), Some(1));
    assert_eq!(first_payload["chapters_created"].as_u64(), Some(1));
    assert_eq!(first_payload["evidence_created"].as_u64(), Some(1));

    // Second ingest: everything reused, nothing duplicated.
    let second = ingest.call(&args).await.expect("second ingest");
    assert!(!second.is_error, "second ingest must not error");
    let second_payload: Value =
        serde_json::from_str(&second.content[0].text.clone().unwrap_or_default())
            .expect("second response JSON");
    assert_eq!(
        second_payload["documents_created"].as_u64(),
        Some(0),
        "re-ingest must not create a duplicate document"
    );
    assert_eq!(
        second_payload["chapters_created"].as_u64(),
        Some(0),
        "re-ingest must not create a duplicate chapter"
    );
    assert_eq!(
        second_payload["evidence_created"].as_u64(),
        Some(0),
        "re-ingest must not create a duplicate evidence row"
    );

    // The store must hold exactly one document/chapter/evidence each. The
    // document title is the attached file's stem (not the source_name).
    let docs = store.list_documents().await.expect("list docs");
    assert_eq!(docs.len(), 1, "exactly one document after two ingests");
    let doc = &docs[0];
    let chapters = store.get_chapter_by_no(doc.id, 1).await.expect("chapter");
    assert!(chapters.is_some(), "exactly one chapter for the document");
    let evidences = store
        .list_evidence_by_document(doc.id)
        .await
        .expect("list evidence");
    assert_eq!(
        evidences.len(),
        1,
        "re-ingest must leave exactly one evidence row, got {}",
        evidences.len()
    );
}

/// Objective: Verify agent_fact_compile persists user facts by default and
/// keeps the agent channel disabled until include_agent_facts=true.
/// Invariants: user_facts_persisted >= 1 with include_agent_facts=false;
/// agent/derived counts are 0 until opted in; agent channel opt-in
/// persists agent + derived facts.
#[tokio::test]
async fn agent_fact_compile_persists_user_facts_and_opt_in_agent() {
    let fact_store = Arc::new(SqliteFactStore::open_in_memory().expect("open fact store"));
    let handler = AgentFactCompileHandler {
        fact_store: fact_store.clone(),
    };
    let messages = serde_json::json!([
        {"role": "user", "content": "I love Rust and plan to rewrite the service."},
        {"role": "assistant", "content": "You like Rust and want to rewrite the service."}
    ]);

    // Default: agent channel disabled.
    let args = serde_json::json!({
        "messages": messages,
        "tenant_id": "test-tenant",
        "user_id": "u1"
    });
    let result = handler.call(&args).await.expect("compile");
    assert!(!result.is_error, "compile must not error");
    let payload: Value = serde_json::from_str(&result.content[0].text.clone().unwrap_or_default())
        .expect("compile response is JSON");
    assert!(
        payload["user_facts_persisted"].as_u64().unwrap_or(0) >= 1,
        "user facts are always persisted"
    );
    assert_eq!(
        payload["agent_facts_persisted"].as_u64(),
        Some(0),
        "agent channel is disabled by default"
    );
    assert_eq!(
        payload["derived_facts_persisted"].as_u64(),
        Some(0),
        "derived channel is disabled by default"
    );

    // Opt-in: agent + derived channels enabled.
    let args = serde_json::json!({
        "messages": messages,
        "tenant_id": "test-tenant",
        "user_id": "u1",
        "agent_id": "a1",
        "include_agent_facts": true
    });
    let result = handler.call(&args).await.expect("compile opt-in");
    let payload: Value = serde_json::from_str(&result.content[0].text.clone().unwrap_or_default())
        .expect("compile opt-in response is JSON");
    assert_eq!(
        payload["include_agent_facts"].as_bool(),
        Some(true),
        "agent channel opted in"
    );
    // User facts are still persisted on the second call.
    assert!(
        payload["user_facts_persisted"].as_u64().unwrap_or(0) >= 1,
        "user facts still persisted with agent channel on"
    );

    // Verify agent entity is distinct from user entity.
    let user_id = payload["user_entity_id"].as_i64().expect("user entity id");
    let agent_id = payload["agent_entity_id"]
        .as_i64()
        .expect("agent entity id");
    assert_ne!(
        user_id, agent_id,
        "user and agent entities must be distinct (zero-pollution)"
    );
}

/// Objective: Verify knowledge_attach rejects an unknown source_type with
/// an error result (not a panic).
/// Invariants: is_error true; message names the bad source_type.
#[tokio::test]
async fn knowledge_attach_rejects_unknown_source_type() {
    let registry = Arc::new(ExternalKnowledgeRegistry::new());
    let linker: SharedEntityLinker = Arc::new(RwLock::new(EntityLinker::new()));
    let handler = KnowledgeAttachHandler { registry, linker };
    let args = serde_json::json!({"source_type": "quantum"});
    let result = handler.call(&args).await.expect("handler does not panic");
    assert!(result.is_error, "unknown source_type yields an error");
    let text = result.content[0].text.clone().unwrap_or_default();
    assert!(text.contains("quantum"), "error names the bad source_type");
}

/// Objective: Verify the attach sandbox refuses a path that lives INSIDE an
/// allowlisted root but names engine configuration or a database. The allowlist
/// is the whole installation directory, so `config/…` and `*.db` files were
/// readable and could be pumped back out through the evidence tools — a file-read
/// primitive rather than an attach (audit H1).
/// Invariants: a corpus file under an allowlisted root still resolves; a
/// `config/` path and a `.db` path are rejected even though both exist.
#[test]
fn attach_path_rejects_engine_configuration_and_databases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_dir = dir.path().join("config");
    std::fs::create_dir(&config_dir).expect("create config dir");
    let secret = config_dir.join("keys.json");
    std::fs::write(&secret, "{\"openai_api_key\": \"sk-secret\"}").expect("write secret");
    let database = dir.path().join("memory.db");
    std::fs::write(&database, b"SQLite format 3\0").expect("write database");

    let corpus = dir.path().join("corpus.txt");
    std::fs::write(&corpus, "刘备很高兴。").expect("write corpus");
    let resolved = resolve_knowledge_path(corpus.to_str().expect("utf-8 path"))
        .expect("a corpus file under an allowlisted root must resolve");
    assert_eq!(
        resolved,
        corpus.canonicalize().expect("canonical corpus path"),
        "the resolved path is the canonical one"
    );

    for denied in [&secret, &database] {
        let error = resolve_knowledge_path(denied.to_str().expect("utf-8 path"))
            .expect_err("engine state must not be attachable");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "{denied:?} must be InvalidInput, got {error:?}"
        );
        assert!(
            error.to_string().contains("not attachable"),
            "the error must state the reason, got: {error}"
        );
    }
}

/// Objective: Verify a symlink cannot smuggle a read out of the sandbox. The
/// check compared path TEXT (`starts_with`), so a link placed in an allowlisted
/// directory passed while the actual read followed it out (audit H1).
/// Invariants: a link that resolves outside every allowlisted root is rejected.
#[cfg(unix)]
#[test]
fn attach_path_rejects_symlinks_that_leave_the_sandbox() {
    let dir = tempfile::tempdir().expect("tempdir");
    let link = dir.path().join("escape.txt");
    std::os::unix::fs::symlink("/etc/hosts", &link).expect("create symlink");

    let error = resolve_knowledge_path(link.to_str().expect("utf-8 path"))
        .expect_err("a link pointing outside the sandbox must be refused");
    assert!(
        matches!(error, Error::InvalidInput(_)),
        "expected InvalidInput, got {error:?}"
    );
    assert!(
        error.to_string().contains("escapes the allowlisted roots"),
        "the error must name the escape, got: {error}"
    );
}

/// Objective: Verify a binary file with an extension the loader does not know is
/// refused instead of being decoded as prose. `detect_format` falls back to
/// `Text`, so `payload.bin` used to be stored as evidence and read back out
/// through the evidence tools (audit H1).
/// Invariants: NUL-bearing content under an unknown extension is InvalidInput; a
/// `.txt` file with the same bytes is treated as text, because the extension
/// declares it.
#[test]
fn attach_path_rejects_binary_content_with_an_unknown_extension() {
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = dir.path().join("payload.bin");
    std::fs::write(&payload, b"\x7fELF\x02\x01\x00\x00binary").expect("write binary");

    let error =
        reject_binary_content(&payload, "payload.bin").expect_err("binary content must be refused");
    assert!(
        matches!(error, Error::InvalidInput(_)),
        "expected InvalidInput, got {error:?}"
    );
    assert!(
        error.to_string().contains("looks binary"),
        "the error must state the reason, got: {error}"
    );

    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "刘备很高兴。").expect("write text");
    reject_binary_content(&text, "notes.txt").expect("text content passes");
}
