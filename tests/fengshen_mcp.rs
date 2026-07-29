//! MCP query: 封神演义 主线
//! Run: cargo test --test fengshen_mcp -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

const DB_PATH: &str = "/tmp/lorescope_sanguo.db";

#[tokio::test]
async fn fengshen_mcp() {
    let k = Arc::new(SQLiteKnowledgeStore::open(DB_PATH).await.unwrap());

    // Try to search evidence for 封神演义
    println!("=== MCP search for 封神演义 ===\n");
    let hits = k
        .search_evidence("封神", Some("封神演义"), 5)
        .await
        .unwrap();
    if hits.is_empty() {
        println!("  NOT FOUND in MCP store");
        println!("  MCP store only has 四大名著 (V1→migration)");
        println!("  封神演义 not ingested.");
    } else {
        for h in &hits {
            println!("  {}", h.text.chars().take(100).collect::<String>());
        }
    }

    // Try broad search
    println!("\n=== MCP search for 姜子牙 ===");
    let hits2 = k.search_evidence("姜子牙", None, 5).await.unwrap();
    if hits2.is_empty() {
        println!("  姜子牙 not found");
    } else {
        for h in &hits2 {
            println!("  {}", h.text.chars().take(100).collect::<String>());
        }
    }
}
