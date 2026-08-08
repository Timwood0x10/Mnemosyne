//! Fix: correct 曹仁's relation from 孙策 to 孙匡.
//! Run: cargo test --test caoren_fix fix_caoren_relation -- --nocapture

mod common;

use mnemosyne::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn fix_caoren_relation() {
    eprintln!("\n========== 修复曹仁关系：孙策→孙匡 ==========\n");

    let db_path = common::ensure_sanguo_db().await;
    let k = Arc::new(SQLiteKnowledgeStore::open(db_path).await.unwrap());

    // 1. Find 曹仁 → 孙策 edge
    let Some(caoren) = k.find_object_by_name("曹仁", None).await.unwrap() else {
        eprintln!("⚠ 曹仁 not found");
        return;
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
        eprintln!("✅ 曹仁→孙策 的边不存在，无需修复");
        return;
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
        let sk_id = k
            .create_object(&mnemosyne::knowledge::KnowledgeObject {
                id: 0,
                doc_id: sunce.doc_id,
                object_type: mnemosyne::knowledge::ObjectType::Person,
                name: "孙匡".into(),
                properties: serde_json::json!({"note": "孙策幼弟，曹仁之女婿"}),
                confidence: 1.0,
                created_at: 0,
            })
            .await
            .unwrap();
        eprintln!("已创建 孙匡 (id={})", sk_id);
        sk_id
    };

    // 3. Use the store's update_edge_target (added to fix the un-wired
    //    correct_relation pipeline) instead of a raw SQL connection, which
    //    bypassed FK enforcement and could corrupt the WAL.
    k.update_edge_target(edge_id, sunkuang_id).await.unwrap();
    eprintln!("已更新 edge {}: target_id → {}", edge_id, sunkuang_id);

    // 4. Verify the fix
    eprintln!("\n========== 验证 ==========\n");
    let edges_after = k.get_edges_touching(caoren.id).await.unwrap();
    for e in &edges_after {
        if let Some(tgt) = k.get_object(e.target_id).await.unwrap() {
            if tgt.name == "孙策" || tgt.name == "孙匡" {
                eprintln!(
                    "  曹仁 --[{}]--> {} (edge_id={})",
                    e.predicate, tgt.name, e.id
                );
            }
        }
    }
    eprintln!("\n========== 修复完成 ==========");
}
