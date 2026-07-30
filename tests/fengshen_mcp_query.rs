//! Ingest 封神演义 into MCP store, then query 孔宣 via MCP tools.
//! Run: cargo test --test fengshen_mcp_query -- --nocapture

use std::sync::Arc;

use lore_scope::compiler::CompileContext;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::{chunk, extract, profile, sentence};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};
use lore_scope::knowledge::{KnowledgeObject, KnowledgeStore, ObjectType, SQLiteKnowledgeStore};

const DB: &str = "/tmp/fengshen_mcp.db";

#[tokio::test]
async fn fengshen_mcp_query() {
    let _ = std::fs::remove_file(DB);
    let k = Arc::new(SQLiteKnowledgeStore::open(DB).await.unwrap());

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

    // 2. Ingest into MCP store
    // First create the document object — doc_id refers to itself
    let doc_obj = KnowledgeObject {
        id: 1,
        doc_id: 1,
        object_type: ObjectType::Concept,
        name: "封神演义".into(),
        properties: serde_json::json!({"source": "corpus/封神演义.txt"}),
        confidence: 1.0,
        created_at: 0,
    };
    let doc_id = k.create_object(&doc_obj).await.unwrap();

    // Create event objects referencing the doc_id
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
        let _ = k.create_object(&obj).await;
    }

    // 3. Query 孔宣 via MCP store
    println!("========== MCP query: 孔宣 ==========\n");
    match k.inspect_entity("孔宣", Some("封神演义")).await.unwrap() {
        Some(r) => {
            println!("Name:     {}", r.object.name);
            println!("Events:   {}", r.events.len());
            println!("Relations: {}", r.relations.len());
            println!("Evidence:  {}\n", r.evidences.len());

            println!("Events:");
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

            println!("\nEvidence:");
            for ev in &r.evidences {
                println!("  {}", ev.content.chars().take(120).collect::<String>());
            }
        }
        None => println!("孔宣 not found in MCP store"),
    }

    // 4. Stats
    println!("\n━━━ Stats ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    // Count objects by type
    let kongxuan = k.find_object_by_name("孔宣", None).await.unwrap();
    println!("Total objects: {}, doc_id: {}", ctx.events.len(), doc_id);
    println!("孔宣: {:?}", kongxuan.map(|o| o.name));

    // Cleanup
    let _ = std::fs::remove_file(DB);
    println!("\n========== COMPLETE ==========");
}
