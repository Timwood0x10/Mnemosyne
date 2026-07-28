use lore_scope::character::{CharacterStore, SQLiteCharacterStore};
use lore_scope::knowledge::KnowledgeStore;
use lore_scope::ingest::IngestionPipeline;
use lore_scope::knowledge::{Migrator, SQLiteKnowledgeStore};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== STEP 1: Create in-memory stores ===");
    let v1_store = Arc::new(SQLiteCharacterStore::open_in_memory().await?);
    let knowledge_store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await?);

    println!("=== STEP 2: Ingest ONLY 三国演义 (much faster) ===");
    let pipeline = IngestionPipeline::new(v1_store.clone(), "corpus");
    let stats = pipeline.distill_novel("三国演义").await?;
    println!("✓ 三国演义 ingested: {} characters, {} events, {} relations", 
             stats.characters, stats.events, stats.relations);

    println!("=== STEP 3: Run migration to Knowledge Object Graph ===");
    let migrator = Migrator::new(&*v1_store, &*knowledge_store, std::path::Path::new("corpus"));
    let migrate_stats = migrator.migrate().await?;
    println!("✓ Migrated: {} documents, {} chapters, {} objects, {} edges, {} evidence, {} mentions",
             migrate_stats.documents, migrate_stats.chapters, 
             migrate_stats.objects, migrate_stats.edges, 
             migrate_stats.evidence, migrate_stats.mentions);

    println!("=== STEP 4: Query 吕布 via inspect_entity ===");
    let result = knowledge_store.inspect_entity("吕布", Some("三国演义")).await?;
    
    match result {
        Some(r) => {
            println!("\n┌──────────────────────────────────────────────┐");
            println!("│  INSPECT ENTITY: 吕布                         │");
            println!("├───────────────────────────────────────────────┤");
            println!("│ Type:        {}", r.object.object_type);
            println!("│ Name:        {}", r.object.title);
            if let Some(f) = r.object.attributes.get("faction") {
                println!("│ Faction:     {}", f.as_str().unwrap_or("unknown"));
            } else {
                println!("│ Faction:     <not set>");
            }
            println!("│ Importance:  {:.2}", r.object.confidence);
            println!("│\n│ Events ({}): ", r.events.len());
            for (i, ev) in r.events.iter().take(5).enumerate() {
                if let Some(ch) = ev.properties.get("chapter").and_then(|c| c.as_i64()) {
                    println!(" │   {}. {} (ch {})", i+1, ev.name, ch);
                } else {
                    println!(" │   {}. {}", i+1, ev.name);
                }
            }
            if r.events.len() > 5 {
                println!(" │   ... (+{} more)", r.events.len() - 5);
            }
            println!("│\n│ Relations ({}): ", r.relations.len());
            for rel in &r.relations {
                let weight = rel.weight();
                let confidence = rel.confidence();
                let predicate = &rel.predicate;
                let target = rel.properties.get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let is_cross = predicate == "君臣" && !rel.properties.get("faction_same").map(|b| b.as_bool()).unwrap_or(false);
                let mark = if is_cross { "[CROSS-DOWNGRADED]" } else { "" };
                println!(" │   {} ─[{}{}]→ {}  [w={:.2}, cf={:.2}] {}", 
                         rel.source_id, predicate, mark, target, weight, confidence, mark);
            }
            println!("│\n│ Evidence count: {}", r.evidences.len());
            println!("└──────────────────────────────────────────────┘");
            
            let has_cross_junchen = r.relations.iter().any(|rel| {
                rel.predicate == "君臣" && 
                !rel.properties.get("faction_same").map(|b| b.unwrap_or(false)).unwrap_or(false)
            });
            
            if has_cross_junchen {
                println!("\n⚠  Warning: Found potential cross-faction 君臣关系");
            } else {
                println!("\n✓ All 君臣 relations are same-faction — faction constraint active!");
            }
        },
        None => println!("✗ 吕布 not found in database!"),
    }

    println!("=== END ===");
    Ok(())
}