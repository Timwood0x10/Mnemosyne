//! 全量语料 generalize 编译 + inspect_entity 验收回归。
//!
//! 对 `corpus/` 下每一部可编译语料（txt 小说 + json 对话）走生产统一链路：
//!
//! ```text
//! DocumentSource → compile_source() → SQLite 图谱 → inspect_entity()
//! ```
//!
//! 断言：
//! - 编译不失败，至少落 1 个 object（文档标题实体）。
//! - `inspect_entity(标题)` 能读回该实体（别名/类型解析可用）。
//! - 小说语料额外验证 NovelProvider 注册路径不 panic（有则校验）。
//!
//! 运行：cargo test --test generalize_corpus_regression -- --nocapture

use lore_scope::compiler::pipeline::compile_source;
use lore_scope::knowledge::KnowledgeStore;
use lore_scope::knowledge::document_source::{DialogSource, RawTextSource};
use lore_scope::knowledge::domain_profile::DomainProfile;
use lore_scope::knowledge::store::SQLiteKnowledgeStore;
use lore_scope::types::Message;

/// 小说语料（.txt，非对话 → 走 corpus 发现 + NovelProvider 字典注册路径）。
const TXT_CORPORA: &[&str] = &[
    "corpus/三国演义.txt",
    "corpus/水浒传.txt",
    "corpus/红楼梦.txt",
    "corpus/西游记.txt",
    "corpus/封神演义.txt",
    "corpus/倾城之恋.txt",
    "corpus/PrideAndPrejudice.txt",
];

/// 对话语料（.json，带 `messages` 数组 → 走 dialog 编译路径）。
const DIALOG_CORPORA: &[(&str, &str)] = &[
    ("corpus/bailiusu_escape.json", "bailiusu"),
    ("corpus/sonia_raskolnikov.json", "sonia"),
    ("corpus/raskolnikov_porfiry.json", "porfiry"),
];

/// 标题（文档名，用作 inspect_entity 的查询键）→ 语料路径。
fn title_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// 逐条验证一个语料：编译 → inspect_entity(标题) 必须读回实体。
async fn verify_corpus(
    label: &str,
    source: &dyn lore_scope::knowledge::document_source::DocumentSource,
    expected_title: &str,
) {
    let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
    let profile = DomainProfile::load("conversation_cognition").expect("profile pack");
    let stats = compile_source(source, &profile, &store, "regression")
        .await
        .unwrap_or_else(|e| panic!("`{label}` compile failed: {e}"));
    assert_eq!(
        stats.documents, 1,
        "`{label}` must produce exactly one document, got {stats:?}"
    );
    assert!(
        stats.objects >= 1,
        "`{label}` must land ≥1 object, got {stats:?}"
    );

    let inspected = store
        .inspect_entity(expected_title, None)
        .await
        .expect("inspect_entity must not error")
        .unwrap_or_else(|| panic!("`{label}`: inspect_entity(`{expected_title}`) found nothing"));
    assert_eq!(
        inspected.object.name, expected_title,
        "`{label}` entity name mismatch"
    );
    println!(
        "  ✓ `{label}`: objects={} edges={} evidence={} | inspect_entity=`{}` events={} relations={}",
        stats.objects,
        stats.edges,
        stats.evidence,
        inspected.object.name,
        inspected.events.len(),
        inspected.relations.len(),
    );
}

/// Objective: Verify the full corpus compiles and inspects end-to-end.
/// Invariants: every novel/dialog corpus yields ≥1 object and a readable
/// entity via `inspect_entity`.
///
/// Marked `#[ignore]` (project convention for slow tests, same as
/// `tests/knowledge_migration.rs`): the 7 full-length novels take ~4 minutes,
/// so this runs explicitly with `cargo test -- --ignored` rather than in the
/// default fast inner loop.
#[tokio::test]
#[ignore = "slow: full-novel corpus compile (~4 min); run explicitly with --ignored"]
async fn full_corpus_compile_and_inspect_regression() {
    println!("\n════════════════════════════════════════════════════════");
    println!("  全量语料 generalize 编译 + inspect_entity 验收回归");
    println!("════════════════════════════════════════════════════════\n");

    // 1. 小说语料：RawTextSource（title=文件名，doc_type="text"）。
    for path in TXT_CORPORA {
        let full = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), path);
        let text =
            std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("read `{path}` failed: {e}"));
        let title = title_of(path);
        let source = RawTextSource::new(title.clone(), *path, text, "text");
        verify_corpus(*path, &source, &title).await;
    }

    // 2. 对话语料：从 JSON 的 `messages` 数组构建 DialogSource。
    for (path, agent_id) in DIALOG_CORPORA {
        let full = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), path);
        let raw =
            std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("read `{path}` failed: {e}"));
        let data: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON corpus");
        let messages = data["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("`{path}` has no messages array"))
            .iter()
            .map(|m| {
                Message::new(
                    m["role"].as_str().unwrap_or("user"),
                    m["content"].as_str().unwrap_or(""),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            !messages.is_empty(),
            "`{path}` messages array must not be empty"
        );
        let source = DialogSource::new(*agent_id, *path, messages);
        verify_corpus(path, &source, agent_id).await;
    }

    println!("\n════════════════════════════════════════════════════════");
    println!(
        "  全量语料验收回归通过（{} 小说 + {} 对话）",
        TXT_CORPORA.len(),
        DIALOG_CORPORA.len()
    );
    println!("════════════════════════════════════════════════════════\n");
}
