//! MCP only: query 孔宣 fate.
//! Run: cargo test --test mcp_kongxuan -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn mcp_kongxuan() {
    let k = Arc::new(SQLiteKnowledgeStore::open("/tmp/fengshen_mcp.db").await.unwrap());
    let hits = k.search_evidence("准提道人收孔宣", None, 5).await.unwrap();
    for h in &hits {
        println!("Ch.{}: {}", h.chapter, h.text);
    }
}
