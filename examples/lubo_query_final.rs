use lore_scope::character::{CharacterStore, SQLiteCharacterStore};
use lore_scope::ingest::IngestionPipeline;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Step 1: Create in-memory store");
    let store = Arc::new(SQLiteCharacterStore::open_in_memory().await?);

    println!("Step 2: Ingesting Romance of the Three Kingdoms only");
    let pipeline = IngestionPipeline::new(store.clone(), "corpus/三国演义.txt");
    let stats = pipeline.run().await?;
    println!("  Ingestion complete: {} chars, {} events, {} relations", 
             stats.characters, stats.events, stats.relations);

    println!("Step 3: Querying 吕布's relations");
    let rels = store.get_relations_for_character("吕布", "novels", Some("三国演义")).await?;
    println!("  Found {} relations for 吕布", rels.len());

    let mut sorted: Vec<_> = rels.into_iter().collect();
    sorted.sort_by(|a, b| b.importance.partial_cmp(&a.importance).unwrap_or(std::cmp::Ordering::Equal));

    println!("\nTop 10 relations by weight:");
    for (i, r) in sorted.iter().take(10).enumerate() {
        let other = if r.source_character == "吕布" { &r.target_character } else { &r.source_character };
        let co_count: u64 = r.metadata.entries.get("co_occurrence_count")
            .and_then(|v| v.as_u64()).unwrap_or(0);
        println!("  {}. {:<15} [{:<8}] weight={:.4} co={}", i+1, other, r.relation_type, r.importance, co_count);
    }

    println!("\nDone.");
    Ok(())
}
