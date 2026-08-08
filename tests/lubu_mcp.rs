//! MCP query: 吕布's life via the knowledge store (same backend as MCP tools).
//! Run: cargo test --test lubu_mcp lubu_query -- --nocapture

use mnemosyne::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn lubu_query() {
    println!("========== MCP 查询：吕布 ==========\n");

    // Open the persistent DB (populated by V1→migration)
    let db_path = "/tmp/lorescope_sanguo.db";
    if !std::path::Path::new(db_path).exists() {
        println!("⚠  DB not found at {db_path}. Run `make migrate` first, or ignore.");
        return;
    }
    let k = Arc::new(SQLiteKnowledgeStore::open(db_path).await.unwrap());

    // 1. inspect_entity
    println!("━━━ inspect_entity(Lu Bu) ─────────────────────────────────\n");
    match k.inspect_entity("吕布", Some("三国演义")).await {
        Ok(Some(r)) => {
            println!("  Name:   {}", r.object.name);
            println!("  Type:   {:?}", r.object.object_type);
            println!("  Events: {}", r.events.len());

            // Timeline
            println!("\n  Timeline:");
            let mut prev_ts = 0i32;
            for ev in &r.events {
                if ev
                    .properties
                    .get("timestamp")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32
                    != prev_ts
                {
                    prev_ts = ev
                        .properties
                        .get("timestamp")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32;
                    println!("    Chapter {}:", prev_ts);
                }
                println!("      {}", ev.name);
            }

            // Relations
            println!("\n  Relations ({}):", r.relations.len());
            for rel in &r.relations {
                println!(
                    "    predicate={}, src={}, tgt={}",
                    rel.predicate, rel.source_id, rel.target_id
                );
            }

            // Evidence
            println!("\n  Evidence ({}):", r.evidences.len());
            for ev in &r.evidences {
                println!("    {}", ev.content.chars().take(120).collect::<String>());
            }
        }
        Ok(None) => println!("  ⚠ 吕布 not found in MCP store"),
        Err(e) => println!("  ⚠  inspect_entity error: {e}"),
    }

    // 2. relation_graph
    println!("\n━━━ relation_graph(Lu Bu, depth=2) ────────────────────────\n");
    match k.relation_graph("吕布", 2, Some("三国演义")).await {
        Ok(Some(g)) => {
            println!("  Nodes ({}):", g.nodes.len());
            for n in &g.nodes {
                println!("    ─ {}", n.name);
            }
        }
        Ok(None) => println!("  ⚠ relation_graph not found"),
        Err(e) => println!("  ⚠  relation_graph error: {e}"),
    }

    // 3. evidence
    println!("\n━━━ evidence(Lu Bu) ━━━━━━━━━━━━━━━━━━━\n");
    let hits = k
        .search_evidence("吕布", Some("三国演义"), 10)
        .await
        .unwrap();
    for h in &hits {
        println!(
            "  Ch.{}: {}",
            h.chapter,
            h.text.chars().take(120).collect::<String>()
        );
    }

    println!("\n========== COMPLETE ==========");
}
