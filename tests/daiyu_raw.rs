//! MCP raw response: dump full inspect_entity result for Lin Daiyu.
//! Run: cargo test --test daiyu_raw -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

const DB_PATH: &str = "/tmp/lorescope_sanguo.db";

#[tokio::test]
async fn daiyu_raw() {
    let k = Arc::new(SQLiteKnowledgeStore::open(DB_PATH).await.unwrap());

    match k.inspect_entity("林黛玉", Some("红楼梦")).await.unwrap() {
        Some(r) => {
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
        None => println!("null"),
    }
}
