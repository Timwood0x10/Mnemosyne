//! 封神演义 full ingest: raw SQL writes, MCP query.
//! Run: cargo test --test fengshen_full -- --nocapture

use lore_scope::compiler::CompileContext;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::{chunk, extract, profile, sentence};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};
use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

const DB: &str = "/tmp/fengshen_mcp.db";

#[tokio::test]
async fn fengshen_full() {
    let _ = std::fs::remove_file(DB);

    // 1. Compile 封神演义
    let doc = Document::from_file("corpus/封神演义.txt").unwrap();
    let text = &doc.text;
    let mut ctx = CompileContext { document_title: "封神演义".into(), ..Default::default() };
    let mut registry = EntityRegistry::new();
    let provider =
        Arc::new(JsonEntityProvider::from_file("config/entity_profiles/fengshen.json").unwrap());
    let obs_config = provider.observation_config();
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();
    profile::extract_profiles(
        text,
        &mut ctx,
        Some(&dict),
        &[],
        &lore_scope::language::ChineseLanguageProvider::new(),
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
    let entity_resolver = EntityResolver::new(AliasResolver::from_pairs(alias_pairs));
    let chunks = chunk::plan(text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();
    let config = extract::Config {
        strong_verbs: obs_config.first().cloned().unwrap_or_default(),
        action_verbs: obs_config.get(2).cloned().unwrap_or_default(),
        ..extract::Config::default()
    };
    extract::compile(
        &mut ctx,
        &sent_texts,
        &dict,
        &config,
        Some(&entity_resolver),
    );

    // 2. Open store to create tables, then use raw SQL for ALL writes
    let _ = Arc::new(SQLiteKnowledgeStore::open(DB).await.unwrap());
    let conn = rusqlite::Connection::open(DB).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

    // Create root doc
    conn.execute(
        "INSERT INTO knowledge_objects (doc_id, object_type, name, properties, confidence, created_at)
         VALUES (1, 'concept', '封神演义', '{}', 1.0, 0)", [],
    ).unwrap();
    let doc_id = conn.last_insert_rowid();
    conn.execute(
        "UPDATE knowledge_objects SET doc_id = ?1 WHERE id = ?1",
        [doc_id],
    )
    .unwrap();

    // Create documents table entry (needed by search_evidence JOIN)
    conn.execute(
        "INSERT INTO documents (id, title, doc_type, created_at)
         VALUES (?1, '封神演义', 'novel', 0)",
        [doc_id],
    )
    .unwrap();

    // Write entities
    let mut entity_count = 0usize;
    for e in &ctx.entities {
        if e.name.len() > 20 {
            continue;
        }
        let props = serde_json::json!({"type": e.entity_type, "status": e.status}).to_string();
        if conn.execute(
            "INSERT INTO knowledge_objects (doc_id, object_type, name, properties, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![doc_id, "person", e.name, props, e.importance],
        ).is_ok() { entity_count += 1; }
    }

    // Write events
    let mut event_count = 0usize;
    for ev in &ctx.events {
        if ev.title.len() > 200 {
            continue;
        }
        let props = serde_json::json!({
            "chapter": ev.timestamp,
            "participants": ev.participants.iter().map(|p| &p.entity_name).collect::<Vec<_>>(),
        })
        .to_string();
        if conn.execute(
            "INSERT INTO knowledge_objects (doc_id, object_type, name, properties, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![doc_id, "event", ev.title, props, ev.importance],
        ).is_ok() { event_count += 1; }
    }

    // Write evidence: ALL text lines as evidence records
    // No sampling — every line that mentions a character name is written.
    let mut evid_count = 0usize;
    let mut chapter_ids: std::collections::HashMap<i32, i64> = std::collections::HashMap::new();
    for (i, line) in text.lines().enumerate() {
        if line.len() < 20 {
            continue;
        }
        // Detect chapter number from "第X回" patterns
        let ch = if let Some(pos) = line.find("第") {
            let rest = &line[pos + 3..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            num_str.parse::<i32>().unwrap_or(i as i32 / 100)
        } else {
            i as i32 / 100
        };
        if let std::collections::hash_map::Entry::Vacant(e) = chapter_ids.entry(ch) {
            let cid = (ch * 100 + 1) as i64;
            e.insert(cid);
            // Create chapters table entry
            let _ = conn.execute(
                "INSERT OR IGNORE INTO chapters (id, doc_id, chapter_no, title, content, start_offset, end_offset)
                 VALUES (?1, ?2, ?3, '', '', 0, 0)",
                rusqlite::params![cid, doc_id, ch],
            );
        }
        let snippet: String = line.chars().take(300).collect();
        if conn
            .execute(
                "INSERT INTO evidence (doc_id, chapter_id, content, created_at)
             VALUES (?1, ?2, ?3, 0)",
                rusqlite::params![doc_id, chapter_ids[&ch], snippet],
            )
            .is_ok()
        {
            evid_count += 1;
        }
    }

    // Re-enable FK
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    drop(conn);

    // 3. Query via MCP store
    let k = Arc::new(SQLiteKnowledgeStore::open(DB).await.unwrap());

    println!("========== MCP query: 孔宣 ==========\n");
    match k.inspect_entity("孔宣", Some("封神演义")).await {
        Ok(Some(r)) => {
            println!("Name:     {}", r.object.name);
            println!("Events:   {}", r.events.len());
            println!("Evidence: {}", r.evidences.len());
            for ev in &r.events {
                let ch = ev
                    .properties
                    .get("chapter")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                println!(
                    "  Ch.{}  {}",
                    ch,
                    ev.name.chars().take(80).collect::<String>()
                );
            }
        }
        Ok(None) => println!("孔宣 not found as entity"),
        Err(e) => println!("inspect_entity error: {e}"),
    }

    // 4. Evidence search
    println!("\n━━━ evidence search: 孔宣 ━━━━━━━━━━━\n");
    let hits = k
        .search_evidence("孔宣", Some("封神演义"), 5)
        .await
        .unwrap();
    for h in &hits {
        println!(
            "  Ch.{}: {}",
            h.chapter,
            h.text.chars().take(120).collect::<String>()
        );
    }

    println!("\n━━━ Stats ━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!(
        "  Entities: {}, Events: {}, Evidence: {}",
        entity_count, event_count, evid_count
    );
    println!("\n========== COMPLETE ==========");
}
