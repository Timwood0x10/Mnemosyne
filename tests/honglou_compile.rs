//! 红楼梦全本编译 + 人物关系网络
//! Run: cargo test --test honglou_compile e2e_honglou -- --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use lore_scope::compiler::CompileContext;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::{chunk, extract, profile, sentence};

#[tokio::test]
async fn e2e_honglou() {
    eprintln!("\n========== 红楼梦 人物关系网络 ==========\n");

    let doc = Document::from_file("corpus/红楼梦.txt").expect("load 红楼梦.txt");
    eprintln!("全文: {} 字符\n", doc.text.len());

    let mut ctx = CompileContext {
        document_title: "红楼梦".into(),
        ..Default::default()
    };

    let mut registry = EntityRegistry::new();
    let provider = Arc::new(
        JsonEntityProvider::from_file("config/entity_profiles/honglou.json")
            .expect("load honglou profile"),
    );
    let obs_config = provider.observation_config();
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();

    profile::extract_profiles(&doc.text, &mut ctx, Some(&dict));

    // Wire Pass 1 → Pass 2: register discovered entities + aliases
    profile::register_discovered_entities(&mut dict, &ctx);

    let chunks = chunk::plan(&doc.text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();

    let config = extract::Config {
        strong_verbs: obs_config.first().cloned().unwrap_or_default(),
        action_verbs: obs_config.get(2).cloned().unwrap_or_default(),
        ..extract::Config::default()
    };
    extract::compile(&mut ctx, &sent_texts, &dict, &config, None);

    eprintln!(
        "━━━ 人物节点 ({} 人) ━━━━━━━━━━━━━━━━━\n",
        ctx.entities.len()
    );
    let mut entity_events: HashMap<String, usize> = HashMap::new();
    for ev in &ctx.events {
        for p in &ev.participants {
            *entity_events.entry(p.entity_name.clone()).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&String, &usize)> = entity_events.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1));
    for (name, count) in ranked.iter().take(20) {
        eprintln!("  {:>4}  {}", count, name);
    }

    eprintln!(
        "\n━━━ 关系网络 ({} 条) ━━━━━━━━━━━━━━━━━\n",
        ctx.relations.len()
    );
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for r in &ctx.relations {
        adj.entry(r.source.clone())
            .or_default()
            .push(r.target.clone());
        adj.entry(r.target.clone())
            .or_default()
            .push(r.source.clone());
    }
    for v in adj.values_mut() {
        v.sort();
        v.dedup();
    }
    let mut names: Vec<&str> = ctx.entities.iter().map(|e| e.name.as_str()).collect();
    names.sort();
    for name in names.iter().take(20) {
        if let Some(conns) = adj.get(*name) {
            if !conns.is_empty() {
                eprintln!("  {} ─── {}", name, conns.join("、"));
            }
        }
    }

    eprintln!(
        "\n━━━ 事件统计 (共 {} 件) ━━━━━━━━━━━━━━\n",
        ctx.events.len()
    );
    let top10: Vec<&(&String, &usize)> = ranked.iter().take(10).collect();
    for (name, count) in top10 {
        eprintln!("  {:>4} 件  {}", count, name);
    }

    assert!(ctx.events.len() > 50, "should extract events");
    eprintln!("\n========== 完成 ==========");
}
