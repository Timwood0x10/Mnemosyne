//! Populate a persistent DB and query 曹仁 via MCP.
//! Run: cargo test --test caoren_mcp setup_and_query -- --nocapture

mod common;

use std::sync::Arc;

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};

#[tokio::test]
async fn setup_and_query() {
    let db_path = common::ensure_sanguo_db().await;

    // DB is already populated by ensure_sanguo_db; open for queries
    let knowledge = Arc::new(SQLiteKnowledgeStore::open(db_path).await.unwrap());

    // ── MCP query: inspect_entity ──────────────────────────────────────
    eprintln!("\n━━━ MCP inspect_entity(曹仁) ━━━━━\n");
    match knowledge
        .inspect_entity("曹仁", Some("三国演义"))
        .await
        .unwrap()
    {
        Some(r) => {
            eprintln!("人物: {} (type={:?})", r.object.name, r.object.object_type);
            eprintln!("\n与之相关的人物 ({} 条):", r.relations.len());
            for rel in &r.relations {
                eprintln!("  ── {} ──> (entity id={})", rel.predicate, rel.target_id);
            }
            eprintln!("\n原文证据 ({} 条):", r.evidences.len());
            for ev in &r.evidences {
                let s: String = ev.content.chars().take(80).collect();
                eprintln!("  {}", s);
            }
        }
        None => eprintln!("⚠ 曹仁 not found"),
    }

    // ── MCP query: relation_graph ──────────────────────────────────────
    eprintln!("\n━━━ MCP relation_graph(曹仁, depth=2) ━━━━━\n");
    match knowledge
        .relation_graph("曹仁", 2, Some("三国演义"))
        .await
        .unwrap()
    {
        Some(g) => {
            eprintln!("关系图节点 ({}):", g.nodes.len());
            for n in &g.nodes {
                eprintln!("  ─ {}", n.name);
            }
            eprintln!("\n关系边 ({}):", g.edges.len());
            for e in &g.edges {
                let src = g
                    .nodes
                    .iter()
                    .find(|n| n.id == e.source_id)
                    .map(|n| n.name.as_str())
                    .unwrap_or("?");
                let tgt = g
                    .nodes
                    .iter()
                    .find(|n| n.id == e.target_id)
                    .map(|n| n.name.as_str())
                    .unwrap_or("?");
                eprintln!("  {} ──[{}]──> {}", src, e.predicate, tgt);
            }
        }
        None => eprintln!("⚠ relation_graph not found"),
    }

    // ── MCP query: evidence search ─────────────────────────────────────
    eprintln!("\n━━━ MCP search_evidence(曹仁) ━━━━━\n");
    let hits = knowledge
        .search_evidence("曹仁", Some("三国演义"), 10)
        .await
        .unwrap();
    for h in &hits {
        eprintln!(
            "  Chapter {}: {} [conf={}]",
            h.chapter,
            h.text.chars().take(80).collect::<String>(),
            h.confidence
        );
    }

    eprintln!("\nDB persisted at: {}", db_path);
}
