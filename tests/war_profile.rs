//! Profile compiler phases on War and Peace sample.
//! Run: cargo test --test war_profile -- --nocapture

use std::sync::Arc;
use std::time::Instant;

use lore_scope::compiler::CompileContext;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::{chunk, extract, profile, sentence};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};

#[tokio::test]
async fn war_profile() {
    // Load first 200K chars to get a representative sample
    let full = std::fs::read_to_string("corpus/WarandPeace.txt").unwrap();
    let text: &str = &full[..full
        .char_indices()
        .nth(50_000)
        .map(|(i, _)| i)
        .unwrap_or(full.len())];
    println!("========== Compiler Profile ==========\n");
    println!("Sample: {} chars\n", text.len());

    // Phase 1: Document
    let t0 = Instant::now();
    let _doc = Document::from_text("War and Peace", "novel", text);
    let t1 = t0.elapsed();

    // Phase 2: Build registry + dict
    let t2_start = Instant::now();
    let mut ctx = CompileContext {
        document_title: "War and Peace".into(),
        ..Default::default()
    };
    let mut registry = EntityRegistry::new();
    let provider =
        Arc::new(JsonEntityProvider::from_file("config/entity_profiles/warandpeace.json").unwrap());
    let obs_config = provider.observation_config();
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();
    let t2 = t2_start.elapsed();

    // Phase 3: Profile extraction
    let t3_start = Instant::now();
    profile::extract_profiles(
        text,
        &mut ctx,
        Some(&dict),
        &[],
        &lore_scope::language::ChineseLanguageProvider::new(),
    );
    let t3 = t3_start.elapsed();

    // Phase 4: Register discovered entities
    let t4_start = Instant::now();
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
    let t4 = t4_start.elapsed();

    // Phase 5: Chunk plan
    let t5_start = Instant::now();
    let chunks = chunk::plan(text, chunk::Config::default());
    let t5 = t5_start.elapsed();

    // Phase 6: Sentence split
    let t6_start = Instant::now();
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();
    let t6 = t6_start.elapsed();

    // Phase 7: Story Compiler
    let config = extract::Config {
        strong_verbs: obs_config.first().cloned().unwrap_or_default(),
        action_verbs: obs_config.get(2).cloned().unwrap_or_default(),
        ..extract::Config::default()
    };
    let t7_start = Instant::now();
    extract::compile(
        &mut ctx,
        &sent_texts,
        &dict,
        &config,
        Some(&entity_resolver),
    );
    let t7 = t7_start.elapsed();

    let total = t1 + t2 + t3 + t4 + t5 + t6 + t7;

    // Report
    println!("Phase                        time        %        ");
    println!("──────────────────────────────────────────────────");
    println!(
        "1. Document load       {:>8.3}s  {:>5.1}%",
        t1.as_secs_f64(),
        t1.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    println!(
        "2. Registry build      {:>8.3}s  {:>5.1}%",
        t2.as_secs_f64(),
        t2.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    println!(
        "3. Profile extract     {:>8.3}s  {:>5.1}%",
        t3.as_secs_f64(),
        t3.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    println!(
        "4. Entity register     {:>8.3}s  {:>5.1}%",
        t4.as_secs_f64(),
        t4.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    println!(
        "5. Chunk plan          {:>8.3}s  {:>5.1}%",
        t5.as_secs_f64(),
        t5.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    println!(
        "6. Sentence split      {:>8.3}s  {:>5.1}%",
        t6.as_secs_f64(),
        t6.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    println!(
        "7. Story Compiler      {:>8.3}s  {:>5.1}%",
        t7.as_secs_f64(),
        t7.as_secs_f64() / total.as_secs_f64() * 100.0
    );
    println!("──────────────────────────────────────────────────");
    println!("Total                  {:>8.3}s", total.as_secs_f64());

    println!(
        "\nStats: {} entities, {} sentences, {} events",
        ctx.entities.len(),
        sent_texts.len(),
        ctx.events.len()
    );
}
