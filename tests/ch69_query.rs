//! MCP: 第六十九回 孔宣兵阻金鸡岭
//! Run: cargo test --test ch69 -- --nocapture
//!
//! Note: Diagnostic test — uses in-memory store. If no data has been seeded
//! (via migration), search returns empty results and the test passes gracefully.

use mnemosyne::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn ch69() {
    let k = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.unwrap());

    println!("========== 第六十九回 孔宣兵阻金鸡岭 ==========\n");

    // Search evidence for this chapter title
    match k.search_evidence("第六十九回", None, 5).await {
        Ok(hits) => {
            for h in &hits {
                println!("{}", h.text.chars().take(200).collect::<String>());
            }
        }
        Err(e) => println!("⚠  search_evidence error: {e}"),
    }

    // Also search for 孔宣 in context
    println!("\n━━━ 孔宣在第六十九回的提及 ────────────────────────\n");
    match k.search_evidence("孔宣兵阻", None, 5).await {
        Ok(hits) => {
            for h in &hits {
                println!("{}", h.text.chars().take(200).collect::<String>());
            }
        }
        Err(e) => println!("⚠  search_evidence error: {e}"),
    }

    // Try broader 孔宣 search
    println!("\n━━━ 孔宣原文摘录 ────────────────────────────────\n");
    match k.search_evidence("孔宣", None, 10).await {
        Ok(hits) => {
            if hits.is_empty() {
                println!("  (no evidence hits for 孔宣)");
            } else {
                for h in &hits {
                    println!("{}", h.text.chars().take(200).collect::<String>());
                }
            }
        }
        Err(e) => println!("⚠  search_evidence error: {e}"),
    }
}
