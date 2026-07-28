use lore_scope::character::SQLiteCharacterStore;
use lore_scope::ingest::IngestionPipeline;
use std::sync::Arc;

#[tokio::test]
async fn bench_ingest() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let corpus = std::fs::read_to_string("corpus/三国演义.txt").unwrap();
    std::fs::write(dir.join("三国演义.txt"), &corpus).unwrap();
    for f in &["水浒传.txt", "红楼梦.txt", "西游记.txt"] {
        std::fs::write(dir.join(f), "").unwrap();
    }

    let store = Arc::new(SQLiteCharacterStore::open_in_memory().await.unwrap());
    let pipeline = IngestionPipeline::new(store.clone(), dir.to_str().unwrap());

    let start = std::time::Instant::now();
    let stats = pipeline.run().await.unwrap();
    let elapsed = start.elapsed();

    eprintln!(
        "三国: {} chars, {} events, {} relations",
        stats.characters, stats.events, stats.relations
    );
    eprintln!("⏱  {:.1}s", elapsed.as_secs_f64());

    assert_eq!(stats.characters, 51);
    assert!(stats.events >= 900, "events >= 900, got {}", stats.events);
    assert!(
        stats.relations >= 700,
        "relations >= 700, got {}",
        stats.relations
    );
}
