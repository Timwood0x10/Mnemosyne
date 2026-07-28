//! Populate a persistent DB and query 曹仁 via MCP.
//! Run: cargo test --test caoren_mcp setup_and_query -- --nocapture

use std::path::Path;
use std::sync::Arc;

use lore_scope::character::SQLiteCharacterStore;
use lore_scope::ingest::IngestionPipeline;
use lore_scope::knowledge::{KnowledgeStore, Migrator, SQLiteKnowledgeStore};

const DB_PATH: &str = "/tmp/lorescope_sanguo.db";

#[tokio::test]
async fn setup_and_query() {
    // Clean any existing DB
    let _ = std::fs::remove_file(DB_PATH);

    // ── V1 ingest ────────────────────────────────────────────────────────
    let v1 = Arc::new(SQLiteCharacterStore::open(DB_PATH).await.unwrap());
    let pipeline = IngestionPipeline::new(v1.clone(), "corpus");
    let istats = pipeline.run().await.expect("ingest");
    eprintln!("V1 ingest: chars={} events={} relations={}",
        istats.characters, istats.events, istats.relations);

    // ── Migrate to general tables ────────────────────────────────────────
    let knowledge = Arc::new(SQLiteKnowledgeStore::open(DB_PATH).await.unwrap());
    let migrator = Migrator::new(&*v1, &knowledge, Path::new("corpus"));
    let mstats = migrator.migrate().await.expect("migrate");
    eprintln!("Migration: objects={} edges={} evidence={} mentions={}",
        mstats.objects, mstats.edges, mstats.evidence, mstats.mentions);

    // ── MCP query: inspect_entity ──────────────────────────────────────
    eprintln!("\n━━━ MCP inspect_entity(曹仁) ━━━━━\n");
    match knowledge.inspect_entity("曹仁", Some("三国演义")).await.unwrap() {
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
    match knowledge.relation_graph("曹仁", 2, Some("三国演义")).await.unwrap() {
        Some(g) => {
            eprintln!("关系图节点 ({}):", g.nodes.len());
            for n in &g.nodes {
                eprintln!("  ─ {}", n.name);
            }
            eprintln!("\n关系边 ({}):", g.edges.len());
            for e in &g.edges {
                let src = g.nodes.iter().find(|n| n.id == e.source_id).map(|n| n.name.as_str()).unwrap_or("?");
                let tgt = g.nodes.iter().find(|n| n.id == e.target_id).map(|n| n.name.as_str()).unwrap_or("?");
                eprintln!("  {} ──[{}]──> {}", src, e.predicate, tgt);
            }
        }
        None => eprintln!("⚠ relation_graph not found"),
    }

    // ── MCP query: evidence search ─────────────────────────────────────
    eprintln!("\n━━━ MCP search_evidence(曹仁) ━━━━━\n");
    let hits = knowledge.search_evidence("曹仁", Some("三国演义"), 10).await.unwrap();
    for h in &hits {
        eprintln!("  Chapter {}: {} [conf={}]", h.chapter, h.text.chars().take(80).collect::<String>(), h.confidence);
    }

    eprintln!("\nDB persisted at: {}", DB_PATH);
}
