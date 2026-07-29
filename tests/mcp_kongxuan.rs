//! MCP-only: query 孔宣 from the store.
//! Run: cargo test --test mcp_kongxuan -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn mcp_kongxuan() {
    let k = Arc::new(SQLiteKnowledgeStore::open("/tmp/lorescope_sanguo.db").await.unwrap());
    match k.inspect_entity("孔宣", None).await.unwrap() {
        Some(r) => {
            println!("Name: {}", r.object.name);
            println!("Events: {}", r.events.len());
            for ev in &r.events { println!("  {}", ev.name.chars().take(80).collect::<String>()); }
        }
        None => println!("孔宣 not found in MCP store"),
    }
}
