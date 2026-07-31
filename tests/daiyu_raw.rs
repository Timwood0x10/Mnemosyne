//! MCP raw response: dump full inspect_entity result for Lin Daiyu.
//! Run: cargo test --test daiyu_raw -- --nocapture
//!
//! Note: This test reads from data seeded by `make migrate`. When run without
//! a seeded database it prints a notice and passes gracefully.

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn daiyu_raw() {
    let db_path = "/tmp/lorescope_sanguo.db";
    // If the pre-seeded DB does not exist, skip gracefully.
    if !std::path::Path::new(db_path).exists() {
        println!("⚠  DB not found at {db_path}. Run `make migrate` first, or ignore.");
        return;
    }
    let k = Arc::new(SQLiteKnowledgeStore::open(db_path).await.unwrap());

    match k.inspect_entity("林黛玉", Some("红楼梦")).await {
        Ok(Some(r)) => {
            println!(
                "object: {}",
                serde_json::to_string_pretty(&r.object).unwrap()
            );
            println!(
                "\nevents: {}",
                serde_json::to_string_pretty(&r.events).unwrap()
            );
            println!("\nrelations ({}):", r.relations.len());
            for rel in &r.relations {
                println!("  {}", serde_json::to_string(rel).unwrap());
            }
            println!("\nevidences ({}):", r.evidences.len());
            for ev in &r.evidences {
                println!("  {}", serde_json::to_string(ev).unwrap());
            }
        }
        Ok(None) => println!("null"),
        Err(e) => println!("⚠  inspect_entity error: {e}"),
    }
}
