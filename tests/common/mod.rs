//! Shared test helpers for integration tests.
//!
//! Provides `ensure_sanguo_db()` which lazily creates and populates the
//! 三国演义 knowledge database. Tests sharing this DB MUST run serially
//! (configured via `.config/nextest.toml` test-group `serial-db`).

use std::path::Path;
use std::sync::Arc;

use lore_scope::character::SQLiteCharacterStore;
use lore_scope::ingest::IngestionPipeline;
use lore_scope::knowledge::{KnowledgeStore, Migrator, SQLiteKnowledgeStore};

/// Shared DB path for integration tests that need the full 三国演义 dataset.
pub const DB_PATH: &str = "/tmp/lorescope_sanguo.db";

/// Ensure the sanguo DB exists and is populated.
///
/// If the DB already contains 曹操 and is writable, it is reused (the setup
/// is expensive, ~40s). Otherwise, the V1 ingestion pipeline + migration
/// are run from scratch.
///
/// # Panics
///
/// Panics if the ingestion or migration fails.
pub async fn ensure_sanguo_db() -> &'static str {
    // Fast path: DB already exists, has data, and is writable
    if Path::new(DB_PATH).exists() {
        if let Ok(k) = SQLiteKnowledgeStore::open(DB_PATH).await {
            if let Ok(Some(_)) = k.find_object_by_name("曹操", None).await {
                // Verify the DB is writable by checking we can open a raw connection
                if rusqlite::Connection::open(DB_PATH)
                    .and_then(|c| {
                        c.busy_timeout(std::time::Duration::from_secs(5))?;
                        c.execute("PRAGMA user_version", [])
                    })
                    .is_ok()
                {
                    return DB_PATH;
                }
            }
        }
        // DB exists but is corrupt/readonly — delete and rebuild
        let _ = std::fs::remove_file(DB_PATH);
    }

    // Slow path: set up the DB from scratch
    let v1 = Arc::new(SQLiteCharacterStore::open(DB_PATH).await.unwrap());
    let pipeline = IngestionPipeline::new(v1.clone(), "corpus");
    pipeline.run().await.expect("V1 ingest");

    let knowledge = Arc::new(SQLiteKnowledgeStore::open(DB_PATH).await.unwrap());
    let migrator = Migrator::new(&*v1, &knowledge, Path::new("corpus"));
    migrator.migrate().await.expect("migrate to knowledge");

    DB_PATH
}
