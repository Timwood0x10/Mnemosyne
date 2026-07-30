//! 封神演义 主线故事 + 重大事件线
//! Run: cargo test --test fengshen_analyze -- --nocapture

use std::collections::HashMap;

use lore_scope::compiler::document::Document;
use lore_scope::compiler::CompileContext;
use lore_scope::compiler::{chunk, extract, profile, sentence};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};

#[tokio::test]
async fn fengshen_main_story() {
    println!("========== 封神演义 · 主线分析 ==========\n");

    let doc = Document::from_file("corpus/封神演义.txt").expect("load 封神演义.txt");
    println!("全文: {} 字符\n", doc.text.len());

    let mut ctx = CompileContext { document_title: "封神演义".into(), ..Default::default() };

    let mut dict = lore_scope::compiler::entity::EntityDictionary::default();
    profile::extract_profiles(
        &doc.text,
        &mut ctx,
        Some(&dict),
        &[],
        &lore_scope::language::ChineseLanguageProvider::new(),
    );

    // Build EntityResolver from discovered entities
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
        .filter_map(|(alias, canonical)| {
            dict.name_to_id
                .get(canonical)
                .map(|id| (alias.clone(), *id))
        })
        .collect();
    let entity_resolver = EntityResolver::new(AliasResolver::from_pairs(alias_pairs));

    let chunks = chunk::plan(&doc.text, chunk::Config::default());
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

    // ── 1. Entity rankings ─────────────────────────────────
    println!("━━━ 1. 主要人物 (事件活跃度前 20) ━━━━━━━━━━━\n");
    let mut entity_event_counts: HashMap<String, usize> = HashMap::new();
    for ev in &ctx.events {
        for p in &ev.participants {
            *entity_event_counts
                .entry(p.entity_name.clone())
                .or_default() += 1;
        }
    }
    let mut ranked: Vec<(&String, &usize)> = entity_event_counts.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1));

    for (name, count) in ranked.iter().take(20) {
        println!("  {:>5}  {}", count, name);
    }

    // ── 2. Timeline by chapter ─────────────────────────────
    println!("\n━━━ 2. 回目事件线 ━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    let mut by_chapter: HashMap<i32, Vec<&str>> = HashMap::new();
    for ev in &ctx.events {
        let ch = ev.timestamp.unwrap_or(0);
        by_chapter.entry(ch).or_default().push(ev.title.as_str());
    }
    let mut chs: Vec<i32> = by_chapter.keys().copied().collect();
    chs.sort();

    let chapter_titles = [
        (1, "纣王女娲宫进香"),
        (5, "姜子牙下山"),
        (11, "哪吒出世"),
        (15, "昆仑山子牙下山"),
        (30, "黄飞虎反五关"),
        (38, "四圣西岐会子牙"),
        (52, "绝龙岭闻仲归天"),
        (65, "殷郊岐山受犁锄"),
        (77, "老子一气化三清"),
        (84, "子牙兵取临潼关"),
        (91, "蟠龙岭烧邬文化"),
        (99, "姜子牙归国封神"),
    ];

    for &(ch, title) in &chapter_titles {
        if let Some(events) = by_chapter.get(&ch) {
            let sample: Vec<&str> = events.iter().take(5).copied().collect();
            println!("  Ch.{:<4} {} — {}", ch, title, sample.join(", "));
        }
    }

    // ── 3. Key events ─────────────────────────────────────
    println!("\n━━━ 3. 关键事件摘录 ━━━━━━━━━━━━━━━━━━━━━━━━\n");
    let key_markers = [
        "封神", "大战", "斩", "死", "破", "擒", "烧", "诛", "瘟", "阵",
    ];
    for marker in &key_markers {
        let hits: Vec<&str> = ctx
            .events
            .iter()
            .filter(|ev| ev.title.contains(marker))
            .map(|ev| ev.title.as_str())
            .take(5)
            .collect();
        if !hits.is_empty() {
            println!("  {} → {}", marker, hits.join(", "));
        }
    }

    // ── 4. Stats ──────────────────────────────────────────
    println!("\n━━━ 4. 统计 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("  实体:     {}", ctx.entities.len());
    println!("  事件:     {}", ctx.events.len());
    println!(
        "  跨度:     Ch.{} ~ Ch.{}",
        chs.first().unwrap_or(&0),
        chs.last().unwrap_or(&0)
    );
    println!("  Top 1:    {:?}", ranked.first());

    assert!(ctx.events.len() > 50, "need events");
    println!("\n========== COMPLETE ==========");
}
