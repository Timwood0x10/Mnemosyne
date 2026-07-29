//! MCP query: 林黛玉's personality profile via the knowledge store.
//! Run: cargo test --test daiyu_query -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use lore_scope::personality::extract_profile;
use std::sync::Arc;

const DB_PATH: &str = "/tmp/lorescope_sanguo.db";

#[tokio::test]
async fn daiyu_query() {
    println!("========== Lin Daiyu · Personality Profile ==========\n");
    let k = Arc::new(SQLiteKnowledgeStore::open(DB_PATH).await.unwrap());

    match k.inspect_entity("林黛玉", Some("红楼梦")).await.unwrap() {
        Some(r) => {
            println!("Name:      {}", r.object.name);
            println!("Type:      {:?}", r.object.object_type);
            println!("Events:    {}", r.events.len());
            println!("Relations: {}", r.relations.len());
            println!("Evidence:  {}\n", r.evidences.len());

            // Extract personality profile from evidence
            let profile = extract_profile("林黛玉", &r.evidences);

            println!("━━━ Personality Traits ━━━━━━━━━━━━━━━━━━━━━\n");
            if profile.traits.is_empty() {
                println!("  (No personality traits detected)");
            } else {
                for t in &profile.traits {
                    let pct = (t.confidence * 100.0) as u32;
                    let snippet: String = t.evidence.chars().take(60).collect();
                    println!("  [{:>3}%] {:12} ← {}", pct, t.name, snippet);
                }
            }
        }
        None => println!("  ⚠ 林黛玉 not found in MCP store"),
    }

    println!("\n========== COMPLETE ==========");
}
