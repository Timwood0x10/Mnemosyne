//! War and Peace → MCP store, then query Anna via MCP.
//! Run: cargo test --test war_mcp -- --nocapture

mod common;

use std::sync::Arc;

use lore_scope::compiler::writer::{EvidenceBatch, EvidenceWriter};
use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};

const DB: &str = "/tmp/warpeace_mcp.db";

#[tokio::test]
async fn war_mcp() {
    let _ = std::fs::remove_file(DB);
    println!("========== War and Peace → MCP Store ==========\n");

    // Full 84k-sentence compile with the warandpeace.json provider is ~2
    // minutes; replay the shared disk cache (mtime-invalidated).
    let compiled = common::ensure_war_mcp_compile();
    println!("Compiled: {} events\n", compiled.events.len());

    // 2. Bootstrap MCP store
    let k_init = Arc::new(SQLiteKnowledgeStore::open(DB).await.unwrap());
    drop(k_init);

    let conn = rusqlite::Connection::open(DB).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    conn.execute(
        "INSERT INTO knowledge_objects (doc_id, object_type, name, properties, confidence, created_at)
         VALUES (1, 'concept', 'War and Peace', '{}', 1.0, 0)", [],
    ).unwrap();
    let doc_id = conn.last_insert_rowid();
    conn.execute(
        "UPDATE knowledge_objects SET doc_id = ?1 WHERE id = ?1",
        [doc_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO documents (id, title, doc_type, created_at)
         VALUES (?1, 'War and Peace', 'novel', 0)",
        [doc_id],
    )
    .unwrap();
    let k = Arc::new(SQLiteKnowledgeStore::open(DB).await.unwrap());

    // 3. Write entities + events + evidence
    let mut ec = 0usize;
    for e in &compiled.entities {
        if e.name.len() > 20 {
            continue;
        }
        let props = serde_json::json!({"type": e.entity_type, "status": e.status}).to_string();
        let _ = conn.execute(
            "INSERT INTO knowledge_objects (doc_id, object_type, name, properties, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![doc_id, "person", e.name, props, e.importance],
        );
        ec += 1;
    }

    let mut evc = 0usize;
    for ev in &compiled.events {
        if ev.title.len() > 200 {
            continue;
        }
        let props = serde_json::json!({
            "chapter": ev.timestamp,
            "participants": ev.participants.iter().map(|p| &p.entity_name).collect::<Vec<_>>(),
        })
        .to_string();
        let _ = conn.execute(
            "INSERT INTO knowledge_objects (doc_id, object_type, name, properties, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![doc_id, "event", ev.title, props, ev.importance],
        );
        evc += 1;
    }

    let mut evidc = 0usize;
    // Evidence needs the raw corpus text; the compile itself is cached, but
    // reading the file for evidence lines is milliseconds.
    let text = std::fs::read_to_string("corpus/WarandPeace.txt").expect("corpus");
    let mut batch: Vec<EvidenceBatch> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.len() < 20 {
            continue;
        }
        let snippet: String = line.chars().take(300).collect();
        batch.push(EvidenceBatch {
            doc_id,
            chapter_id: (i / 100) as i64,
            content: snippet,
        });
        if batch.len() >= 1000 {
            evidc += EvidenceWriter::default().write(&conn, &batch);
            batch.clear();
        }
    }
    if !batch.is_empty() {
        evidc += EvidenceWriter::default().write(&conn, &batch);
    }
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    drop(conn);

    println!("Entities: {}, Events: {}, Evidence: {}\n", ec, evc, evidc);

    // 4. MCP query: Anna
    println!("========== MCP query: Anna ==========\n");
    match k.inspect_entity("Anna", Some("War and Peace")).await {
        Ok(Some(r)) => {
            println!("Name:     {}", r.object.name);
            println!("Events:   {}", r.events.len());
            for ev in &r.events {
                println!("  {}", ev.name.chars().take(80).collect::<String>());
            }
        }
        Ok(None) => println!("Anna not found as entity — searching evidence..."),
        Err(e) => println!("inspect_entity error: {e}"),
    }

    println!("\n━━━ evidence search: Anna ━━━━━━━━━━━━━━\n");
    let hits = k
        .search_evidence("Anna", Some("War and Peace"), 10)
        .await
        .unwrap();
    for h in &hits {
        println!("  {}", h.text.chars().take(120).collect::<String>());
    }

    println!("\n========== COMPLETE ==========");
}
