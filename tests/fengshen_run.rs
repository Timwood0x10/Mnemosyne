//! 封神演义 主线分析 — with fengshen.json config profile
//! Run: cargo test --test fengshen_run -- --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use lore_scope::compiler::CompileContext;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::{chunk, extract, profile, sentence};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};

#[tokio::test]
async fn fengshen_run() {
    println!("========== 封神演义 · 主线分析 ==========\n");

    let doc = Document::from_file("corpus/封神演义.txt").unwrap();
    println!("全文: {} 字符\n", doc.text.len());

    let mut ctx = CompileContext::default();
    ctx.document_title = "封神演义".into();

    // Load JSON config profile
    let mut registry = EntityRegistry::new();
    let provider = Arc::new(
        JsonEntityProvider::from_file("config/entity_profiles/fengshen.json").unwrap(),
    );
    let obs_config = provider.observation_config();
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();

    profile::extract_profiles(&doc.text, &mut ctx, Some(&dict), &[], &lore_scope::language::ChineseLanguageProvider::new());

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

    // ── 人物活跃度 ─────────────────────────────────
    println!("━━━ 主要人物（事件活跃度 Top 15）━━━━━━━━━\n");
    let mut counts: HashMap<String, usize> = HashMap::new();
    for ev in &ctx.events {
        for p in &ev.participants {
            *counts.entry(p.entity_name.clone()).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&String, &usize)> = counts.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1));
    for (name, count) in ranked.iter().take(15) {
        println!("  {:>5}  {}", count, name);
    }

    // ── 回目事件线 ─────────────────────────────────
    println!("\n━━━ 关键回目事件 ━━━━━━━━━━━━━━━━━━━━━━━━\n");
    let mut by_ch: HashMap<i32, Vec<&str>> = HashMap::new();
    for ev in &ctx.events {
        by_ch.entry(ev.timestamp.unwrap_or(0)).or_default().push(ev.title.as_str());
    }
    let mut chs: Vec<i32> = by_ch.keys().copied().collect();
    chs.sort();

    let key_chapters = [1, 5, 11, 15, 30, 38, 52, 65, 77, 84, 91, 99];
    for &ch in &key_chapters {
        if let Some(evts) = by_ch.get(&ch) {
            let sample: Vec<&str> = evts.iter().take(5).copied().collect();
            println!("  Ch.{:<4} {}", ch, sample.join(", "));
        }
    }

    // ── 关键事件搜索 ────────────────────────────────
    println!("\n━━━ 重大事件 ━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    let markers = ["封神", "斩", "大战", "破", "擒", "诛仙", "瘟", "阵", "死", "化"];
    for m in &markers {
        let hits: Vec<&str> = ctx.events.iter()
            .filter(|ev| ev.title.contains(m))
            .map(|ev| ev.title.as_str())
            .take(4).collect();
        if !hits.is_empty() {
            println!("  {}  →  {}", m, hits.join(" | "));
        }
    }

    // ── 统计 ───────────────────────────────────────
    println!("\n━━━ 统计 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("  实体: {}", ctx.entities.len());
    println!("  事件: {}", ctx.events.len());
    println!("  跨度: Ch.{} ~ Ch.{}", chs.first().unwrap_or(&0), chs.last().unwrap_or(&0));

    assert!(ctx.events.len() > 50);
    println!("\n========== COMPLETE ==========");
}
