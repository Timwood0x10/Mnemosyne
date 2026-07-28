//! End-to-end test: compile 三国演义 chapter 1 through Pass 1 + Pass 2.
//!
//! Run: cargo test --test sanguo_compile e2e_sanguo -- --nocapture

use std::sync::Arc;

use lore_scope::compiler::chunk;
use lore_scope::compiler::entity::{
    EntityEngine, EntityRegistry, JsonEntityProvider,
};
use lore_scope::compiler::sentence;
use lore_scope::compiler::{extract, profile};
use lore_scope::compiler::document::Document;
use lore_scope::compiler::CompileContext;

#[tokio::test]
async fn e2e_sanguo() {
    eprintln!("\n========== 三国演义 编译测试 ==========\n");

    // ── Load text ──────────────────────────────────────────────────────
    let doc = Document::from_file("corpus/三国演义.txt")
        .expect("load 三国演义.txt");
    // Take first ~12KB (first chapter+)
    let text: String = doc.text.chars().take(12000).collect();
    eprintln!("Text length: {} chars", text.len());

    // ── Pass 1: Profile Extractor ──────────────────────────────────────
    let mut ctx = CompileContext::default();
    ctx.document_title = "三国演义".into();

    // Extract profiles from character introduction sections
    // The first chapter contains many introductions
    profile::extract_profiles(&text, &mut ctx);
    eprintln!("\n[Pass 1] Profiles extracted:");
    for p in &ctx.profiles {
        eprintln!("  {} → {}: {}", 
            ctx.entities.iter().find(|e| e.id == p.entity_id).map(|e| e.name.as_str()).unwrap_or("?"),
            p.key, p.value);
    }
    eprintln!("  Total entities: {}", ctx.entities.len());
    eprintln!("  Total profiles: {}", ctx.profiles.len());

    // ── Build Entity Registry for Pass 2 ───────────────────────────────
    let mut registry = EntityRegistry::new();
    let provider = Arc::new(
        JsonEntityProvider::from_file("config/entity_profiles/sanguo.json")
            .expect("load sanguo profile"),
    );
    let obs_config = provider.observation_config();
    registry.register(provider);

    // Assign entity IDs from profile extraction
    let dict = registry.build_dictionary();
    let name_to_id: std::collections::HashMap<String, i64> = ctx.entities.iter()
        .filter_map(|e| e.id.map(|id| (e.name.clone(), id)))
        .collect();
    // (In production, IDs come from the DB. For the test we use sequential ids.)
    let synthetic_ids: std::collections::HashMap<String, i64> = ctx.entities.iter()
        .enumerate()
        .map(|(i, e)| (e.name.clone(), (i + 1) as i64))
        .collect();

    // ── Pass 2: Story Compiler ─────────────────────────────────────────
    let chunks = chunk::plan(&text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();

    eprintln!("\n[Pass 2] Processing {} sentences...", sent_texts.len());

    let config = extract::Config {
        strong_verbs: obs_config.get(0).cloned().unwrap_or_default(),
        action_verbs: obs_config.get(2).cloned().unwrap_or_default(),
        ..extract::Config::default()
    };

    extract::compile(&mut ctx, &sent_texts, &dict, &config);
    eprintln!("  Events: {}", ctx.events.len());
    eprintln!("  Relations: {}", ctx.relations.len());

    // ── Results ────────────────────────────────────────────────────────
    eprintln!("\n========== 结果汇总 ==========");
    eprintln!("实体: {}", ctx.entities.len());
    eprintln!("画像属性: {}", ctx.profiles.len());
    eprintln!("事件: {}", ctx.events.len());
    eprintln!("关系: {}", ctx.relations.len());

    // Print entities
    eprintln!("\n--- 人物节点 ---");
    for e in &ctx.entities {
        let profs: Vec<&str> = ctx.profiles.iter()
            .filter(|p| p.entity_id == e.id)
            .map(|p| p.value.as_str())
            .collect();
        eprintln!("  {} [{}]", e.name, profs.join(", "));
    }

    // Print events
    eprintln!("\n--- 事件 ---");
    for ev in &ctx.events {
        let parts: Vec<&str> = ev.participants.iter()
            .map(|p| p.entity_name.as_str())
            .collect();
        eprintln!("  [{}] {} (参与者: {})", ev.event_type, ev.title, parts.join(", "));
    }

    // Print relations
    eprintln!("\n--- 关系 ---");
    for r in &ctx.relations {
        eprintln!("  {} --[{}]--> {}", r.source, r.relation_type, r.target);
    }

    // ── Assertions ─────────────────────────────────────────────────────
    // We should have found at least some events from strong verb matching
    assert!(!ctx.events.is_empty() || sent_texts.len() < 5,
        "should extract at least some events from the text");

    // If profiles were found, we should have entities
    if !ctx.profiles.is_empty() {
        assert!(!ctx.entities.is_empty(), "profiles imply entities exist");
    }

    eprintln!("\n========== 测试完成 ==========\n");
}
