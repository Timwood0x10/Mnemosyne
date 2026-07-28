//! Example: ingest the real corpus and print 吕布's relationship network.
use std::sync::Arc;

use lore_scope::character::{CharacterStore, SQLiteCharacterStore, traverse_character_network};
use lore_scope::ingest::IngestionPipeline;

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

    // === 2. Look up 吕布 ===
    let profile = store
        .search_characters_by_name("吕布", TENANT, Some("三国演义"))
        .await?;
    let lubo = profile
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("吕布 not found in 三国演义 after ingestion"))?;
    eprintln!(
        "→ 吕布 profile: novel={}, aliases={:?}, importance={:.3}",
        lubo.novel, lubo.aliases, lubo.importance
    );
    eprintln!(
        "  clothing: {}",
        if lubo.clothing.is_empty() {
            "(none)"
        } else {
            &lubo.clothing
        }
    );
    eprintln!(
        "  personality: {}",
        if lubo.personality.is_empty() {
            "(none)"
        } else {
            &lubo.personality
        }
    );
    eprintln!("  description: {}", lubo.description);

    // === 3. List relations with dimensional scoring ===
    let rels = store
        .get_relations_for_character("吕布", TENANT, Some("三国演义"))
        .await?;
    eprintln!("\n→ 吕布 relations ({} edges):", rels.len());

    // Sort by importance descending so the strongest relations come first.
    let mut sorted: Vec<_> = rels.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for r in &sorted {
        let other = if r.source_character == "吕布" {
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

    // === 4. Traverse the network at depth 2 ===
    eprintln!("\n→ Network traversal (depth=2):");
    let root = traverse_character_network(&*store, "吕布", TENANT, Some("三国演义"), 2).await?;
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

    println!("\n✓ Done. {} relations listed for 吕布.", sorted.len());
    Ok(())
}
