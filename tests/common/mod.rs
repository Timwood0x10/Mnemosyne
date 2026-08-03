//! Shared test helpers for integration tests.
//!
//! Provides `ensure_sanguo_db()` which lazily creates and populates the
//! 三国演义 knowledge database. Tests sharing this DB MUST run serially
//! (configured via `.config/nextest.toml` test-group `serial-db`).

use std::path::Path;
use std::sync::Arc;

use lore_scope::character::SQLiteCharacterStore;
use lore_scope::compiler::CompileContext;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::EntityDictionary;
use lore_scope::compiler::{chunk, extract, profile, sentence};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};
use lore_scope::ingest::IngestionPipeline;
use lore_scope::knowledge::{KnowledgeStore, Migrator, SQLiteKnowledgeStore};
use serde::{Deserialize, Serialize};

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
                // Verify the DB is writable by checking we can open a raw
                // connection. NOTE: `PRAGMA user_version` returns a row, so it
                // must be read with `query_row` — using `execute` (which
                // expects a row count) always fails, which made the fast path
                // never match and forced a full ~30s rebuild on every run.
                if rusqlite::Connection::open(DB_PATH)
                    .and_then(|c| {
                        c.busy_timeout(std::time::Duration::from_secs(5))?;
                        c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?;
                        Ok(())
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

/// Cache file for the full War-and-Peace compile (entities/events/relations).
///
/// Compiling the full 84k-sentence English corpus takes ~2 minutes per test
/// binary; both `war_peace` and `war_mcp` compile it. This helper serializes
/// the compiled IR once and replays it on subsequent runs, keyed on the
/// corpus file's mtime so a corpus change invalidates the cache.
/// Cache file for the full War-and-Peace compile (entities/events/relations).
/// Public so tests can report which cache they replayed.
pub const WAR_CACHE_PATH: &str = "/tmp/lorescope_war_compile.json";
/// Corpus file the War-and-Peace cache is keyed on (mtime invalidation).
pub const WAR_CORPUS: &str = "corpus/WarandPeace.txt";

/// Serializable slice of the War-and-Peace compile result.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WarCompile {
    pub entities: Vec<lore_scope::compiler::Entity>,
    pub events: Vec<lore_scope::compiler::Event>,
    pub relations: Vec<lore_scope::compiler::Relation>,
    pub profiles: Vec<lore_scope::compiler::EntityProfile>,
}

/// Compile War and Peace (or replay the disk cache when valid).
///
/// # Panics
///
/// Panics if the corpus is missing or compilation fails.
pub fn ensure_war_compile() -> WarCompile {
    // Fast path: cache exists and the corpus has not changed since it was
    // written (mtime in the file's `_meta`). Saves ~2 minutes per test.
    let corpus_mtime = std::fs::metadata(WAR_CORPUS)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
    if let Ok(raw) = std::fs::read_to_string(WAR_CACHE_PATH) {
        if let Ok(cached) = serde_json::from_str::<WarCompile>(&raw) {
            if let Ok(meta) = std::fs::metadata(WAR_CACHE_PATH) {
                let cache_mtime = meta.modified().ok().map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                });
                // Cache is stale if the corpus changed AFTER the cache was
                // written.
                if cache_mtime.unwrap_or(0) >= corpus_mtime.unwrap_or(u64::MAX) {
                    return cached;
                }
            }
        }
    }

    // Slow path: full compile, then persist.
    let doc = Document::from_file(WAR_CORPUS).expect("WarandPeace.txt");
    let text = &doc.text;
    let mut ctx = CompileContext {
        document_title: "War and Peace".into(),
        ..Default::default()
    };

    let mut dict = EntityDictionary::default();
    profile::extract_profiles(
        text,
        &mut ctx,
        Some(&dict),
        &[],
        &lore_scope::language::EnglishLanguageProvider::new(),
    );
    for entity in &ctx.entities {
        let aliases: Vec<&str> = ctx
            .profiles
            .iter()
            .filter(|p| p.entity_id == entity.id)
            .filter(|p| p.key == "courtesy_name" || p.key == "title")
            .map(|p| p.value.as_str())
            .collect();
        dict.register_discovered(&entity.name, &aliases);
    }
    profile::register_discovered_entities(&mut dict, &ctx);
    let alias_pairs: Vec<(String, i64)> = dict
        .alias_to_canonical
        .iter()
        .filter_map(|(a, c)| dict.name_to_id.get(c).map(|id| (a.clone(), *id)))
        .collect();
    let resolver = EntityResolver::new(AliasResolver::from_pairs(alias_pairs));

    let chunks = chunk::plan(text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();
    let config =
        extract::Config::from_language(&lore_scope::language::EnglishLanguageProvider::new());
    extract::compile(&mut ctx, &sent_texts, &dict, &config, Some(&resolver));

    let compiled = WarCompile {
        entities: ctx.entities,
        events: ctx.events,
        relations: ctx.relations,
        profiles: ctx.profiles,
    };
    if let Ok(json) = serde_json::to_string(&compiled) {
        let _ = std::fs::write(WAR_CACHE_PATH, json);
    }
    compiled
}

/// Cache file for the war_mcp variant (JsonEntityProvider-backed compile).
///
/// `war_mcp` uses `config/entity_profiles/warandpeace.json` via
/// `JsonEntityProvider`, so its IR differs from `ensure_war_compile()`'s
/// default-dictionary compile. Keep a separate cache keyed on the same corpus
/// mtime; a corpus change invalidates BOTH.
const WAR_MCP_CACHE_PATH: &str = "/tmp/lorescope_war_mcp_compile.json";

/// Compile War and Peace with the warandpeace.json entity provider (or replay
/// the disk cache). Mirrors `tests/war_mcp.rs`'s original compile block.
///
/// # Panics
///
/// Panics if the corpus or provider config is missing, or compilation fails.
pub fn ensure_war_mcp_compile() -> WarCompile {
    use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
    use std::sync::Arc;

    let corpus_mtime = std::fs::metadata(WAR_CORPUS)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
    if let Ok(raw) = std::fs::read_to_string(WAR_MCP_CACHE_PATH) {
        if let Ok(cached) = serde_json::from_str::<WarCompile>(&raw) {
            if let Ok(meta) = std::fs::metadata(WAR_MCP_CACHE_PATH) {
                let cache_mtime = meta.modified().ok().map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                });
                if cache_mtime.unwrap_or(0) >= corpus_mtime.unwrap_or(u64::MAX) {
                    return cached;
                }
            }
        }
    }

    let doc = Document::from_file(WAR_CORPUS).expect("WarandPeace.txt");
    let text = &doc.text;
    let mut ctx = CompileContext {
        document_title: "War and Peace".into(),
        ..Default::default()
    };

    let mut registry = EntityRegistry::new();
    let provider = Arc::new(
        JsonEntityProvider::from_file("config/entity_profiles/warandpeace.json")
            .expect("warandpeace.json provider"),
    );
    let obs_config = provider.observation_config();
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();

    let patterns = provider.profile_patterns();
    profile::extract_profiles(
        text,
        &mut ctx,
        Some(&dict),
        &patterns,
        &lore_scope::language::EnglishLanguageProvider::new(),
    );
    for entity in &ctx.entities {
        let aliases: Vec<&str> = ctx
            .profiles
            .iter()
            .filter(|p| p.entity_id == entity.id)
            .filter(|p| p.key == "courtesy_name" || p.key == "title")
            .map(|p| p.value.as_str())
            .collect();
        dict.register_discovered(&entity.name, &aliases);
    }
    profile::register_discovered_entities(&mut dict, &ctx);
    let alias_pairs: Vec<(String, i64)> = dict
        .alias_to_canonical
        .iter()
        .filter_map(|(a, c)| dict.name_to_id.get(c).map(|id| (a.clone(), *id)))
        .collect();
    let resolver = EntityResolver::new(AliasResolver::from_pairs(alias_pairs));

    let chunks = chunk::plan(text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();
    let config = extract::Config {
        strong_verbs: obs_config.first().cloned().unwrap_or_default(),
        action_verbs: obs_config.get(2).cloned().unwrap_or_default(),
        ..extract::Config::default()
    };
    extract::compile(&mut ctx, &sent_texts, &dict, &config, Some(&resolver));

    let compiled = WarCompile {
        entities: ctx.entities,
        events: ctx.events,
        relations: ctx.relations,
        profiles: ctx.profiles,
    };
    if let Ok(json) = serde_json::to_string(&compiled) {
        let _ = std::fs::write(WAR_MCP_CACHE_PATH, json);
    }
    compiled
}
