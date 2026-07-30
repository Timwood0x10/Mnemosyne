//! MCP query: 孔宣 from fengshen store.
//! Run: cargo test --test find_kongxuan -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn find_kongxuan() {
    let k = Arc::new(
        SQLiteKnowledgeStore::open("/tmp/fengshen_mcp.db")
            .await
            .unwrap(),
    );

    // 1. Try inspect_entity without doc filter
    println!("========== 孔宣 ==========\n");
    match k.inspect_entity("孔宣", None).await {
        Ok(Some(r)) => {
            println!("Name:     {}", r.object.name);
            println!("Events:   {}", r.events.len());
            println!("Evidence: {}\n", r.evidences.len());
            for ev in &r.events {
                let ch = ev
                    .properties
                    .get("chapter")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                println!(
                    "  Ch.{:<4} {}",
                    ch,
                    ev.name.chars().take(80).collect::<String>()
                );
            }
        }
        Ok(None) => println!("Not found as entity — searching evidence..."),
        Err(e) => println!("Error: {e}"),
    }

    // 2. Evidence search
    println!("\n━━━ Evidence search ━━━━━━━━━━━━━━━━━\n");
    let hits = k
        .search_evidence("孔宣", Some("封神演义"), 10)
        .await
        .unwrap();
    if hits.is_empty() {
        println!("  (no evidence hits)");
    } else {
        for h in &hits {
            println!(
                "  Ch.{}: {}",
                h.chapter,
                h.text.chars().take(120).collect::<String>()
            );
        }
    }

    // 3. Find by name directly
    println!("\n━━━ find_object_by_name ━━━━━━━━━━━━\n");
    match k.find_object_by_name("孔宣", None).await.unwrap() {
        Some(o) => println!("Found: id={}, type={:?}", o.id, o.object_type),
        None => println!("孔宣 not in knowledge_objects"),
    }
}
