//! End-to-end test: compile full 三国演义 (120 chapters), output character relationship network.
//!
//! Run: cargo test --test sanguo_compile e2e_sanguo -- --nocapture 2>&1 | head -200

use std::collections::HashMap;
use std::sync::Arc;

use lore_scope::compiler::CompileContext;
use lore_scope::compiler::chunk;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::sentence;
use lore_scope::compiler::{extract, profile};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};

#[tokio::test]
async fn e2e_sanguo() {
    eprintln!("\n========== 三国演义 人物关系网络 ==========\n");

    // ── Load full text ──────────────────────────────────────────────────
    let doc = Document::from_file("corpus/三国演义.txt").expect("load 三国演义.txt");
    let chapter_count = doc.text.matches("第").filter(|_| true).count();
    eprintln!("全文: {} 字符, ~{} 回\n", doc.text.len(), chapter_count);

    // ── Build Entity Registry ───────────────────────────────────────────
    let mut ctx = CompileContext {
        document_title: "三国演义".into(),
        ..Default::default()
    };
    let mut registry = EntityRegistry::new();
    let provider = Arc::new(
        JsonEntityProvider::from_file("config/entity_profiles/sanguo.json")
            .expect("load sanguo profile"),
    );
    let obs_config = provider.observation_config();
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();

    // ── Pass 1: Profile Extractor ────────────────────────────────────────
    profile::extract_profiles(&doc.text, &mut ctx, Some(&dict), &[]);

    // Wire Pass 1 → Pass 2: register discovered entities + aliases
    profile::register_discovered_entities(&mut dict, &ctx);

    // Build EntityResolver from the dictionary's alias map
    let alias_pairs: Vec<(String, i64)> = dict
        .alias_to_canonical
        .iter()
        .filter_map(|(alias, canonical)| {
            dict.name_to_id
                .get(canonical)
                .map(|id| (alias.clone(), *id))
        })
        .collect();
    let alias_resolver = AliasResolver::from_pairs(alias_pairs);
    let entity_resolver = EntityResolver::new(alias_resolver);

    // ── Pass 2: Story Compiler ──────────────────────────────────────────
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

    // ── 人物节点 ────────────────────────────────────────────────────────
    eprintln!(
        "━━━ 人物节点 ({} 人) ━━━━━━━━━━━━━━━━━━━━━━━━━\n",
        ctx.entities.len()
    );
    for e in &ctx.entities {
        let profs: Vec<String> = ctx
            .profiles
            .iter()
            .filter(|p| p.entity_id == e.id)
            .map(|p| format!("{}:{}", p.key, p.value))
            .collect();
        let ev_count = ctx
            .events
            .iter()
            .filter(|ev| ev.participants.iter().any(|p| p.entity_name == e.name))
            .count();
        let rel_count = ctx
            .relations
            .iter()
            .filter(|r| r.source == e.name || r.target == e.name)
            .count();
        if !profs.is_empty() || ev_count > 0 {
            eprintln!("  {}  (事件: {}, 关系: {})", e.name, ev_count, rel_count);
            for p in &profs {
                eprintln!(
                    "    ├ {}: {}",
                    p.split(':').next().unwrap_or(""),
                    p.split(':').skip(1).collect::<Vec<_>>().join(":")
                );
            }
        }
    }

    // ── 关系网络 ────────────────────────────────────────────────────────
    eprintln!(
        "\n━━━ 关系网络 ({} 条) ━━━━━━━━━━━━━━━━━━━━━━━━━\n",
        ctx.relations.len()
    );

    // Build adjacency: for each entity, list connected entities
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for r in &ctx.relations {
        adjacency
            .entry(r.source.clone())
            .or_default()
            .push(r.target.clone());
        adjacency
            .entry(r.target.clone())
            .or_default()
            .push(r.source.clone());
    }
    // Deduplicate each entity's connection list
    for v in adjacency.values_mut() {
        v.sort();
        v.dedup();
    }

    let mut names: Vec<&str> = ctx.entities.iter().map(|e| e.name.as_str()).collect();
    names.sort();
    for name in &names {
        if let Some(conns) = adjacency.get(*name) {
            if !conns.is_empty() {
                eprintln!("  {} ─── {}", name, conns.join("、"));
            }
        }
    }

    // ── 事件统计 ────────────────────────────────────────────────────────
    eprintln!(
        "\n━━━ 事件统计 (共 {} 件) ━━━━━━━━━━━━━━━━━━━━━━━\n",
        ctx.events.len()
    );

    let mut event_entity_counts: HashMap<String, usize> = HashMap::new();
    for ev in &ctx.events {
        for p in &ev.participants {
            *event_entity_counts
                .entry(p.entity_name.clone())
                .or_default() += 1;
        }
    }
    let mut ranked: Vec<(&String, &usize)> = event_entity_counts.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1));

    for (name, count) in ranked.iter().take(10) {
        eprintln!("  {:>4} 件  {}", count, name);
    }

    // ── 关键事件摘录 ──────────────────────────────────────────────────
    eprintln!("\n━━━ 关键事件摘录 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let key_events = [
        "杀董卓",
        "斩华雄",
        "斩颜良",
        "诛文丑",
        "过五关",
        "赤壁",
        "借东风",
        "空城计",
        "七擒孟获",
        "六出祁山",
        "失街亭",
        "斩马谡",
        "五丈原",
    ];
    for kw in &key_events {
        let hits: Vec<&str> = ctx
            .events
            .iter()
            .filter(|ev| ev.title.contains(kw) || ev.description.contains(kw))
            .map(|ev| ev.title.as_str())
            .take(3)
            .collect();
        if !hits.is_empty() {
            eprintln!("  {:8} → {}", kw, hits.join(", "));
        }
    }

    // ── 断言 ────────────────────────────────────────────────────────────
    assert!(
        ctx.events.len() > 100,
        "should extract at least 100 events from full text"
    );
    eprintln!("\n========== 测试完成 ==========\n");
}
