//! 孔宣 MCP-style query — from compiler results, without MCP store.
//! Run: cargo test --test kongxuan_query -- --nocapture

use std::sync::Arc;

use lore_scope::compiler::CompileContext;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::{chunk, extract, profile, sentence};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};

#[tokio::test]
async fn kongxuan_query() {
    println!("========== MCP-style query: 孔宣 ==========\n");

    let doc = Document::from_file("corpus/封神演义.txt").unwrap();
    let mut ctx = CompileContext::default();
    ctx.document_title = "封神演义".into();

    let mut registry = EntityRegistry::new();
    let provider = Arc::new(
        JsonEntityProvider::from_file("config/entity_profiles/fengshen.json").unwrap(),
    );
    let obs_config = provider.observation_config();
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();

    profile::extract_profiles(&doc.text, &mut ctx, Some(&dict), &[]);
    for entity in &ctx.entities {
        let aliases: Vec<&str> = ctx.profiles.iter()
            .filter(|p| p.entity_id == entity.id)
            .filter(|p| p.key == "courtesy_name" || p.key == "title")
            .map(|p| p.value.as_str())
            .collect();
        dict.register_discovered(&entity.name, &aliases);
    }
    profile::register_discovered_entities(&mut dict, &ctx);
    let alias_pairs: Vec<(String, i64)> = dict.alias_to_canonical.iter()
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
    extract::compile(&mut ctx, &sent_texts, &dict, &config, Some(&entity_resolver));

    // Query 孔宣 events (MCP-style)
    let kongxuan_events: Vec<_> = ctx.events.iter()
        .filter(|ev| ev.title.contains("孔宣") || ev.participants.iter().any(|p| p.entity_name == "孔宣"))
        .collect();

    if kongxuan_events.is_empty() {
        println!("  ⚠ 孔宣 not found in compiled events");
        println!("  (Possible: not in JSON entity list, auto-discovery missed him)");
    } else {
        println!("  Events: {}", kongxuan_events.len());
        println!("\n  Timeline:");
        for ev in &kongxuan_events {
            let ts = ev.timestamp.unwrap_or(0);
            let parts: Vec<&str> = ev.participants.iter().map(|p| p.entity_name.as_str()).collect();
            println!("    Ch.{:<4} {} [{}]", ts, ev.title, parts.join(", "));
        }
    }

    // Also search raw text for context
    println!("\n━━━ Raw text evidence ━━━━━━━━━━━━━━━━━━━\n");
    for (i, line) in doc.text.lines().enumerate() {
        if line.contains("孔宣") && i < 5 {
            println!("  {}", line.chars().take(120).collect::<String>());
        }
    }

    println!("\n========== COMPLETE ==========");
}
