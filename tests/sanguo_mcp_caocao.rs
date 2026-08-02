//! MCP query: 曹操's growth trajectory via the knowledge store.
//!
//! Uses the same backend as the MCP `inspect_entity` / `timeline` tools.
//! Run: cargo test --test sanguo_mcp_caocao -- --nocapture

mod common;

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::test]
async fn caocao_growth_trajectory() {
    // ensure_sanguo_db runs the V1 ingestion + migration, populating the
    // knowledge store with 三国演义 entities/events/evidence (includes 曹操).
    let db_path = common::ensure_sanguo_db().await;
    let knowledge = Arc::new(SQLiteKnowledgeStore::open(db_path).await.unwrap());

    println!("========== 曹操 · 成长轨迹 (MCP) ==========\n");

    // ── 1. inspect_entity ──────────────────────────────────────────────
    println!("━━━ inspect_entity(曹操) ━━━━━━━━━━━━━━━━\n");
    match knowledge
        .inspect_entity("曹操", Some("三国演义"))
        .await
        .unwrap()
    {
        Some(r) => {
            println!(
                "  人物: {} (type={:?})",
                r.object.name, r.object.object_type
            );
            println!("  事件数: {}", r.events.len());
            println!("  关系数: {}", r.relations.len());
            println!("  证据数: {}\n", r.evidences.len());

            println!("  ── 相关关系 ──");
            for rel in &r.relations {
                println!(
                    "    {} ──{}──> #{}",
                    rel.predicate, rel.predicate, rel.target_id
                );
            }
            println!("\n  ── 原文证据（节选）──");
            for ev in &r.evidences {
                let s: String = ev.content.chars().take(90).collect();
                println!("    {}", s);
            }
        }
        None => println!("  ⚠ 曹操 not found in MCP store"),
    }

    // ── 2. timeline (growth trajectory, sorted by chapter) ─────────────
    println!("\n━━━ timeline(曹操) — 成长轨迹 ━━━━━━━━━━━━\n");
    let entries = knowledge
        .entity_timeline("曹操", Some("三国演义"))
        .await
        .unwrap();
    println!("  共 {} 个时间点\n", entries.len());
    let mut prev_chapter = 0i32;
    for e in &entries {
        // chapter is Option (None when the edge has no valid_from, NEW-K12).
        let ch = e.chapter.unwrap_or(0);
        if ch != prev_chapter {
            println!("  ▸ 第 {} 回:", ch);
            prev_chapter = ch;
        }
        println!("      {}  [{}]", e.event, e.predicate);
    }

    println!("\n========== COMPLETE ==========");
}
