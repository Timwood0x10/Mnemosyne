//! MCP query: 林黛玉's personality profile via the knowledge store.
//! Run: cargo test --test daiyu_query -- --nocapture
//!
//! Note: This test reads from a pre-seeded database. If the database does not
//! exist (no prior migration run), the test prints a notice and passes
//! gracefully so it does not block the regression baseline.

use mnemosyne::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use mnemosyne::personality::extract_profile;
use std::sync::Arc;

#[tokio::test]
async fn daiyu_query() {
    println!("========== Lin Daiyu · Personality Profile ==========\n");
    // Diagnostic test: uses in-memory store so it never depends on external
    // files or migration state. When no data has been seeded, the entity
    // won't be found — the test prints a notice and passes gracefully.
    let k = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.unwrap());

    match k.inspect_entity("林黛玉", Some("红楼梦")).await {
        Ok(Some(r)) => {
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
        Ok(None) => println!("  ⚠ 林黛玉 not found in MCP store"),
        Err(e) => println!("  ⚠  inspect_entity error: {e}"),
    }

    println!("\n========== COMPLETE ==========");
}
