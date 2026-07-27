use std::sync::Arc;
use memory_distill::character::{CharacterStore, SQLiteCharacterStore};
use memory_distill::ingest::IngestionPipeline;

const TENANT: &str = "novels";

#[tokio::test]
async fn query_gaoshun_relations() {
    let path = "/tmp/test_gaoshun.db";
    let _ = std::fs::remove_file(path);
    let store = Arc::new(
        SQLiteCharacterStore::open(path)
            .await
            .expect("open store"),
    );
    let pipeline = IngestionPipeline::new(store.clone(), "corpus");
    eprintln!("starting pipeline...");
    let stats = pipeline.run().await.expect("run pipeline on real corpus");
    eprintln!("done: {} chars, {} events, {} relations", stats.characters, stats.events, stats.relations);

    let rels = store
        .get_relations_for_character("高顺", TENANT, Some("三国演义"))
        .await
        .expect("get 高顺 relations");

    println!("\n=== 高顺的人际关系 ===");
    if rels.is_empty() {
        println!("（无）");
        return;
    }
    for r in &rels {
        let other = if r.source_character == "高顺" { &r.target_character } else { &r.source_character };
        let faction_bonus = r.metadata.entries.get("faction_bonus").and_then(|v| v.as_f64()).unwrap_or(1.0);
        println!(
            "  {} —[{}]→ {}  (importance={:.3}, co_occur={}, faction_bonus={})",
            r.source_character, r.relation_type, r.target_character, r.importance,
            r.metadata.entries.get("co_occurrence_count").and_then(|v| v.as_u64()).unwrap_or(0),
            faction_bonus,
        );
    }
}
