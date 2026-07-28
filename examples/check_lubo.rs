use lore_scope::character::{CharacterStore, SQLiteCharacterStore};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Try to open existing db, else use in-memory
    let store = match SQLiteCharacterStore::open("./memory.db").await {
        Ok(s) => Arc::new(s),
        Err(_) => {
            println!("No ./memory.db, using in-memory store");
            Arc::new(SQLiteCharacterStore::open_in_memory().await?)
        }
    };

    // Search for 吕布 in Romance of the Three Kingdoms
    let chars = store
        .search_characters("吕布", "novels", Some("三国演义"), 10)
        .await?;
    if chars.is_empty() {
        println!(
            "吕布 not found in database. Run character_ingest first (or check that corpus is available)."
        );
        return Ok(());
    }

    for c in &chars {
        println!(
            "Found: {} (aliases: {:?}, importance: {})",
            c.name, c.aliases, c.importance
        );
    }

    // Get relations for 吕布
    let rels = store
        .get_relations_for_character("吕布", "novels", Some("三国演义"))
        .await?;
    println!("\n吕布's relations: {} total", rels.len());

    // Print all relations sorted by importance
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
        let co_occurrence: u64 = r
            .metadata
            .entries
            .get("co_occurrence_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        println!(
            "  {:<15} [{:<8}] weight={:.4} co_occurrences={}",
            other, r.relation_type, r.importance, co_occurrence
        );
    }

    Ok(())
}
