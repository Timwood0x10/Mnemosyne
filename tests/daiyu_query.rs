//! MCP query: 林黛玉's personality profile via the knowledge store.
//! Run: cargo test --test daiyu_query -- --nocapture
//!
//! Note: This test reads from a pre-seeded database. If the database does not
//! exist (no prior migration run), the test prints a notice and passes
//! gracefully so it does not block the regression baseline.

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use lore_scope::personality::extract_profile;
use std::sync::Arc;

#[tokio::test]
async fn daiyu_query() {
    println!("========== Lin Daiyu · Personality Profile ==========\n");
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
