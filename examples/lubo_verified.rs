use lore_scope::character::CharacterStore;
use lore_scope::character::SQLiteCharacterStore;
use lore_scope::faction;
use lore_scope::ingest::IngestionPipeline;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(SQLiteCharacterStore::open_in_memory().await?);

    println!("→ Starting corpus ingestion...");
    let pipeline = IngestionPipeline::new(store.clone(), "corpus");
    let stats = pipeline.run().await?;
    println!(
        "✓ Ingestion complete: {} chars, {} events, {} relations",
        stats.characters, stats.events, stats.relations
    );

    // Get relations for 吕布
    let rels = store
        .get_relations_for_character("吕布", "novels", Some("三国演义"))
        .await?;
    println!("\n吕布's relations: {} total", rels.len());

    let mut sorted: Vec<_> = rels.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    println!("\n--- Relations sorted by weight (top 15) ---");
    for r in sorted.iter().take(15) {
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
        let relation_type = &r.relation_type;
        let is_cross_faction_junchen =
            relation_type == "君臣" && !faction::same_faction_or_unknown("三国演义", "吕布", other);
        let highlight = if is_cross_faction_junchen {
            " [CROSS-FACTION - DOWNGRADED]"
        } else {
            ""
        };
        println!(
            "  {} <-> {} [{}] weight={:.4} co={}{}",
            other,
            if r.source_character == "吕布" {
                "吕布"
            } else {
                "吕布反向"
            },
            relation_type,
            r.importance,
            co_occurrence,
            highlight
        );
    }

    println!("\n--- Checking for false 君臣 relations ---");
    let junchen: Vec<_> = sorted
        .iter()
        .filter(|r| r.relation_type == "君臣")
        .collect();
    if junchen.is_empty() {
        println!("✓ No 君臣 relations found for 吕布 (correct!)");
    } else {
        println!("✗ Found {} 君臣 relation(s):", junchen.len());
        for r in &junchen {
            let other = if r.source_character == "吕布" {
                &r.target_character
            } else {
                &r.source_character
            };
            let cross = !faction::same_faction_or_unknown("三国演义", "吕布", other);
            println!(
                "  吕布 <-> {} (weight: {}, cross-faction: {})",
                other, r.importance, cross
            );
        }
    }

    Ok(())
}
