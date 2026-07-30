//! War and Peace — try compiler pipeline (English novel).
//! Run: cargo test --test war_peace -- --nocapture

use std::collections::HashMap;

use lore_scope::compiler::CompileContext;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::{chunk, extract, profile, sentence};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};

#[tokio::test]
async fn war_peace() {
    println!("========== War and Peace · Analysis ==========\n");

    let doc = Document::from_file("corpus/WarandPeace.txt").unwrap();
    let text = &doc.text;
    println!("Text: {} chars\n", text.len());

    let mut ctx = CompileContext { document_title: "War and Peace".into(), ..Default::default() };

    // Try with empty dict — English text won't match Chinese patterns
    let mut dict = lore_scope::compiler::entity::EntityDictionary::default();
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

    // Use default verbs — these are Chinese, but let's see what happens
    let config = extract::Config::default();
    extract::compile(
        &mut ctx,
        &sent_texts,
        &dict,
        &config,
        Some(&entity_resolver),
    );

    // Stats
    let mut counts: HashMap<String, usize> = HashMap::new();
    for ev in &ctx.events {
        for p in &ev.participants {
            *counts.entry(p.entity_name.clone()).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&String, &usize)> = counts.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1));

    println!("━━━ Events & entities ━━━━━━━━━━━━━━━━━━━\n");
    println!("Entities:  {}", ctx.entities.len());
    println!("Events:    {}", ctx.events.len());
    println!("Sentences: {}\n", sent_texts.len());

    println!("Top 15 by event count:\n");
    for (name, count) in ranked.iter().take(15) {
        println!("  {:>5}  {}", count, name);
    }

    if !ctx.events.is_empty() {
        println!("\nSample events:\n");
        for ev in ctx.events.iter().take(10) {
            let ch = ev.timestamp.unwrap_or(0);
            let parts: Vec<&str> = ev
                .participants
                .iter()
                .map(|p| p.entity_name.as_str())
                .collect();
            println!(
                "  Ch.{}  {} [{}]",
                ch,
                ev.title.chars().take(60).collect::<String>(),
                parts.join(", ")
            );
        }
    }

    println!("\n========== COMPLETE ==========");
}
