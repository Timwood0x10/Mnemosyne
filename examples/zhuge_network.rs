//! Example: ingest the real corpus and print 诸葛亮's relationship network.
//!
//! Run with: `cargo run --example zhuge_network`
//!
//! Re-indexes the four classical novels into an in-memory store, then
//! queries 诸葛亮's character profile, events, and relations — surfacing
//! the dimensional scoring (co-occurrence, event coupling, type) on each
//! edge so the network can be inspected without spinning up the MCP server.

use std::sync::Arc;

use memory_distill::character::{CharacterStore, SQLiteCharacterStore, traverse_character_network};
use memory_distill::ingest::IngestionPipeline;

const TENANT: &str = "novels";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // === 1. Re-index the corpus ===
    eprintln!("→ Re-indexing corpus (corpus/) ...");
    let store = Arc::new(SQLiteCharacterStore::open_in_memory().await?);
    let pipeline = IngestionPipeline::new(store.clone(), "corpus");
    let stats = pipeline.run().await?;
    eprintln!(
        "  ingested: {} characters, {} events, {} relations",
        stats.characters, stats.events, stats.relations
    );

    // === 2. Look up 诸葛亮 (canonical name; 孔明 is an alias) ===
    let profile = store
        .search_characters_by_name("诸葛亮", TENANT, Some("三国演义"))
        .await?;
    let zhuge = profile
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("诸葛亮 not found in 三国演义 after ingestion"))?;
    eprintln!(
        "→ 诸葛亮 profile: novel={}, aliases={:?}, importance={:.3}",
        zhuge.novel, zhuge.aliases, zhuge.importance
    );
    eprintln!(
        "  clothing: {}",
        if zhuge.clothing.is_empty() {
            "(none)"
        } else {
            &zhuge.clothing
        }
    );
    eprintln!(
        "  personality: {}",
        if zhuge.personality.is_empty() {
            "(none)"
        } else {
            &zhuge.personality
        }
    );
    eprintln!("  description: {}", zhuge.description);

    // === 3. List relations with dimensional scoring ===
    let rels = store
        .get_relations_for_character("诸葛亮", TENANT, Some("三国演义"))
        .await?;
    eprintln!("\n→ 诸葛亮 relations ({} edges):", rels.len());

    // Sort by importance descending so the strongest relations come first.
    let mut sorted: Vec<_> = rels.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for r in &sorted {
        let other = if r.source_character == "诸葛亮" {
            &r.target_character
        } else {
            &r.source_character
        };
        let co = r
            .metadata
            .entries
            .get("co_occurrence_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let ev = r
            .metadata
            .entries
            .get("event_coupling_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let ty = r
            .metadata
            .entries
            .get("relation_type_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let co_count = r
            .metadata
            .entries
            .get("co_occurrence_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let shared = r
            .metadata
            .entries
            .get("shared_event_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        eprintln!(
            "  • {:<8} [{:<4}] weight={:.3}  co={:.2}({}章)  ev={:.2}({}事件)  ty={:.2}  (第{}回检测)",
            other, r.relation_type, r.importance, co, co_count, ev, shared, ty, r.chapter
        );
    }

    // === 4. Traverse the network at depth 2 and summarize connections ===
    eprintln!("\n→ Network traversal (depth=2):");
    let root = traverse_character_network(&*store, "诸葛亮", TENANT, Some("三国演义"), 2).await?;
    eprintln!(
        "  root: {} ({} events, {} relations)",
        root.character.name,
        root.events.len(),
        root.relations.len()
    );
    eprintln!("  direct connections:");
    for conn in &root.connections {
        eprintln!(
            "    — {} ({} events, {} relations, {} 2nd-hop neighbors)",
            conn.character.name,
            conn.events.len(),
            conn.relations.len(),
            conn.connections.len()
        );
    }

    println!("\n✓ Done. {} relations listed for 诸葛亮.", sorted.len());
    Ok(())
}
