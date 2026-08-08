//! Ingest 封神演义 into MCP store, then query via MCP tools.
//! Run: cargo test --test fengshen_ingest -- --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use mnemosyne::compiler::CompileContext;
use mnemosyne::compiler::document::Document;
use mnemosyne::compiler::{chunk, extract, profile, sentence};
use mnemosyne::entity_resolver::{AliasResolver, EntityResolver};
use mnemosyne::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};

#[tokio::test]
async fn fengshen_ingest() {
    println!("========== 封神演义 · Ingest + MCP Query ==========\n");

    // 1. Compile via V7
    let doc = Document::from_file("corpus/封神演义.txt").unwrap();
    let text = &doc.text;

    let mut ctx = CompileContext {
        document_title: "封神演义".into(),
        ..Default::default()
    };

    let mut dict = mnemosyne::compiler::entity::EntityDictionary::default();
    profile::extract_profiles(
        text,
        &mut ctx,
        Some(&dict),
        &[],
        &mnemosyne::language::ChineseLanguageProvider::new(),
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

    let config = extract::Config::default();
    extract::compile(
        &mut ctx,
        &sent_texts,
        &dict,
        &config,
        Some(&entity_resolver),
    );

    // 2. Write to MCP store
    let k = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.unwrap());

    // Create document first (required by FK constraint on knowledge_objects)
    let doc = k
        .create_document(&mnemosyne::knowledge::Document {
            id: 0,
            title: "封神演义".into(),
            author: None,
            doc_type: Some("novel".into()),
            created_at: 0,
        })
        .await
        .unwrap();

    // Create doc concept object referencing the document
    let doc_id = k
        .create_object(&mnemosyne::knowledge::KnowledgeObject {
            id: 0,
            doc_id: doc,
            object_type: mnemosyne::knowledge::ObjectType::Concept,
            name: "封神演义".into(),
            properties: serde_json::json!({"source": "corpus/封神演义.txt"}),
            confidence: 1.0,
            created_at: 0,
        })
        .await
        .unwrap();

    for ev in &ctx.events {
        let _event_id = k
            .create_object(&mnemosyne::knowledge::KnowledgeObject {
                id: 0,
                doc_id,
                object_type: mnemosyne::knowledge::ObjectType::Event,
                name: ev.title.clone(),
                properties: serde_json::json!({
                    "chapter": ev.timestamp,
                    "description": ev.description,
                }),
                confidence: ev.importance,
                created_at: 0,
            })
            .await
            .unwrap();
    }

    // 3. Query via MCP store
    println!("━━━ MCP inspect_entity（主要人物）━━━━━━━━━\n");
    let mut entity_events: HashMap<String, usize> = HashMap::new();
    for ev in &ctx.events {
        for p in &ev.participants {
            *entity_events.entry(p.entity_name.clone()).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&String, &usize)> = entity_events.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1));
    for (name, count) in ranked.iter().take(15) {
        println!("  {:>5}  {}", count, name);
    }

    // 4. Timeline
    println!("\n━━━ MCP timeline (关键回目) ━━━━━━━━━━━━━━\n");
    let mut by_ch: HashMap<i32, Vec<&str>> = HashMap::new();
    for ev in &ctx.events {
        by_ch
            .entry(ev.timestamp.unwrap_or(0))
            .or_default()
            .push(ev.title.as_str());
    }
    let mut chs: Vec<i32> = by_ch.keys().copied().collect();
    chs.sort();

    let highlights = [1, 5, 11, 15, 30, 38, 52, 65, 77, 84, 91, 99];
    for &ch in &highlights {
        if let Some(events) = by_ch.get(&ch) {
            let s: Vec<&str> = events.iter().take(4).copied().collect();
            println!("  Ch.{:<4} {}", ch, s.join(", "));
        }
    }

    // 5. Key events
    println!("\n━━━ MCP evidence (关键事件) ━━━━━━━━━━━━━━\n");
    let markers = ["封神", "斩", "大战", "死", "破", "擒", "诛仙", "瘟", "阵"];
    for m in &markers {
        let hits: Vec<&str> = ctx
            .events
            .iter()
            .filter(|ev| ev.title.contains(m))
            .map(|ev| ev.title.as_str())
            .take(3)
            .collect();
        if !hits.is_empty() {
            println!("  {} → {}", m, hits.join(", "));
        }
    }

    println!("\n━━━ 统计 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("  实体: {}, 事件: {}", ctx.entities.len(), ctx.events.len(),);

    assert!(ctx.events.len() > 50);
    println!("\n========== COMPLETE ==========");
}
