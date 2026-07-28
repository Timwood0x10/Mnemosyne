//! Fix: correct 曹仁's relation from 孙策 to 孙匡.
//! Run: cargo test --test caoren_fix fix_caoren_relation -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

const DB_PATH: &str = "/tmp/lorescope_sanguo.db";

#[tokio::test]
async fn fix_caoren_relation() {
    eprintln!("\n========== 修复曹仁关系：孙策→孙匡 ==========\n");

    let k = Arc::new(SQLiteKnowledgeStore::open(DB_PATH).await.unwrap());

    // 1. Find 曹仁 → 孙策 edge
    let Some(caoren) = k.find_object_by_name("曹仁", None).await.unwrap() else {
        eprintln!("⚠ 曹仁 not found"); return;
    };
    let edges = k.get_edges_touching(caoren.id).await.unwrap();
    let mut target_edge = None;
    for e in &edges {
        if let Some(tgt) = k.get_object(e.target_id).await.unwrap() {
            if tgt.name == "孙策" && e.predicate == "关联" {
                target_edge = Some(e.id);
            }
        }
    }
    let Some(edge_id) = target_edge else {
        eprintln!("✅ 曹仁→孙策 的边不存在，无需修复"); return;
    };
    eprintln!("找到曹仁→孙策边: edge_id={}", edge_id);

    // 2. Check if 孙匡 exists; if not, create it
    let sunkuang_id = if let Some(sk) = k.find_object_by_name("孙匡", None).await.unwrap() {
        eprintln!("孙匡 已存在 (id={})", sk.id);
        sk.id
    } else {
        // Find 孙策's doc_id to use for 孙匡
        let sunce = k.find_object_by_name("孙策", None).await.unwrap().unwrap();
        // Create 孙匡 object
        let sk_id = k.create_object(&lore_scope::knowledge::KnowledgeObject {
            id: 0,
            doc_id: sunce.doc_id,
            object_type: lore_scope::knowledge::ObjectType::Person,
            name: "孙匡".into(),
            properties: serde_json::json!({"note": "孙策幼弟，曹仁之女婿"}),
            confidence: 1.0,
            created_at: 0,
        }).await.unwrap();
        eprintln!("已创建 孙匡 (id={})", sk_id);
        sk_id
    };

    // 3. Direct SQL: update the edge's target_id
    // Open a raw SQLite connection for the update
    let conn = rusqlite::Connection::open(DB_PATH).unwrap();
    conn.execute(
        "UPDATE knowledge_edges SET target_id = ?1 WHERE id = ?2",
        rusqlite::params![sunkuang_id, edge_id],
    ).unwrap();
    eprintln!("已更新 edge {}: target_id → {}", edge_id, sunkuang_id);

    // 4. Verify the fix
    eprintln!("\n========== 验证 ==========\n");
    let edges_after = k.get_edges_touching(caoren.id).await.unwrap();
    for e in &edges_after {
        if let Some(tgt) = k.get_object(e.target_id).await.unwrap() {
            if tgt.name == "孙策" || tgt.name == "孙匡" {
                eprintln!("  曹仁 --[{}]--> {} (edge_id={})", e.predicate, tgt.name, e.id);
            }
        }
    }
    eprintln!("\n========== 修复完成 ==========");
}
