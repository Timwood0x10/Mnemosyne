//! MCP: 第六十九回 孔宣兵阻金鸡岭
//! Run: cargo test --test ch69 -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn ch69() {
    let k = Arc::new(
        SQLiteKnowledgeStore::open("/tmp/fengshen_mcp.db")
            .await
            .unwrap(),
    );

    println!("========== 第六十九回 孔宣兵阻金鸡岭 ==========\n");

    // Search evidence for this chapter title
    let hits = k.search_evidence("第六十九回", None, 5).await.unwrap();
    for h in &hits {
        println!("{}", h.text.chars().take(200).collect::<String>());
    }

    // Also search for 孔宣 in context
    println!("\n━━━ 孔宣在第六十九回的提及 ━━━━━━━━━━━━━━\n");
    let hits2 = k.search_evidence("孔宣兵阻", None, 5).await.unwrap();
    for h in &hits2 {
        println!("{}", h.text.chars().take(200).collect::<String>());
    }

    // Try broader 孔宣 search
    println!("\n━━━ 孔宣原文摘录 ━━━━━━━━━━━━━━━━━━━━━\n");
    let hits3 = k.search_evidence("孔宣", None, 10).await.unwrap();
    if hits3.is_empty() {
        println!("  (no evidence hits for 孔宣)");
    } else {
        for h in &hits3 {
            println!("{}", h.text.chars().take(200).collect::<String>());
        }
    }
}
