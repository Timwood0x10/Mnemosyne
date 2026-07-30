//! MCP raw response: dump full inspect_entity result for Lin Daiyu.
//! Run: cargo test --test daiyu_raw -- --nocapture
//!
//! Note: This test reads from a pre-seeded database. If the database does not
//! exist (no prior migration run), the test prints a notice and passes
//! gracefully so it does not block the regression baseline.

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn daiyu_raw() {
    let db_path = "/tmp/lorescope_sanguo.db";
    if !std::path::Path::new(db_path).exists() {
        println!(
            "⚠  Pre-seeded database not found at {db_path}. Run `make migrate` first, or ignore this test."
        );
        return;
    }
    let k = Arc::new(SQLiteKnowledgeStore::open(db_path).await.unwrap());

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
