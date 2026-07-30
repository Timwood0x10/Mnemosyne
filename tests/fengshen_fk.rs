//! 封神演义 ingest into MCP store (FK fixed), then query 孔宣.
//! Run: cargo test --test fengshen_fk -- --nocapture

use std::sync::Arc;

use lore_scope::compiler::CompileContext;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::{chunk, extract, profile, sentence};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};
use lore_scope::knowledge::{KnowledgeObject, KnowledgeStore, ObjectType, SQLiteKnowledgeStore};

#[tokio::test]
async fn fengshen_fk() {
    // Use in-memory store to avoid /tmp file-system race conditions
    let k = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.unwrap());

    // Create document first (required by FK constraint on knowledge_objects)
    let doc_id = k
        .create_document(&lore_scope::knowledge::Document {
            id: 0,
            title: "封神演义".into(),
            author: None,
            doc_type: Some("novel".into()),
            created_at: 0,
        })
        .await
        .unwrap();

    // 1. Compile
    let doc = Document::from_file("corpus/封神演义.txt").unwrap();
    let mut ctx = CompileContext {
        document_title: "封神演义".into(),
        ..Default::default()
    };

    let mut registry = EntityRegistry::new();
    let provider =
        Arc::new(JsonEntityProvider::from_file("config/entity_profiles/fengshen.json").unwrap());
    let obs_config = provider.observation_config();
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();

    profile::extract_profiles(
        &doc.text,
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

    let chunks = chunk::plan(&doc.text, chunk::Config::default());
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

    // 2. Ingest events into MCP store
    let mut written = 0usize;
    for ev in &ctx.events {
        let title = &ev.title;
        if title.len() > 200 {
            continue;
        }
        let obj = KnowledgeObject {
            id: 0,
            doc_id,
            object_type: ObjectType::Event,
            name: title.to_string(),
            properties: serde_json::json!({
                "chapter": ev.timestamp,
                "participants": ev.participants.iter().map(|p| &p.entity_name).collect::<Vec<_>>(),
            }),
            confidence: ev.importance,
            created_at: 0,
        };
        if k.create_object(&obj).await.is_ok() {
            written += 1;
        }
    }

    // 3. MCP query: 孔宣
    println!("========== MCP query: 孔宣 ==========\n");
    match k.inspect_entity("孔宣", Some("封神演义")).await.unwrap() {
        Some(r) => {
            println!("Name:     {}", r.object.name);
            println!("Events:   {}", r.events.len());
            println!("Evidence: {}\n", r.evidences.len());
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
        None => println!("孔宣 not found in MCP store"),
    }

    println!("\n━━━ Stats ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!(
        "Compiled events: {}, Ingested: {}, Doc ID: {}",
        ctx.events.len(),
        written,
        doc_id
    );

    println!("\n========== COMPLETE ==========");
}
