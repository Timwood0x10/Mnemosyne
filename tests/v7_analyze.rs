//! V7 Pipeline：全本分析 → 时间线关系网络
//! Run: cargo test --test v7_analyze full_analysis -- --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use lore_scope::compiler::{chunk, sentence, extract, profile, timeline, faction};
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::{CompileContext, Event, Relation, EventParticipant};

#[tokio::test]
async fn full_analysis() {
    eprintln!("\n========== V7 三国演义 时间线分析 ==========\n");

    let doc = Document::from_file("corpus/三国演义.txt").unwrap();
    let text = &doc.text;

    let mut ctx = CompileContext::default();
    ctx.document_title = "三国演义".into();

    let mut registry = EntityRegistry::new();
    let provider = Arc::new(
        JsonEntityProvider::from_file("config/entity_profiles/sanguo.json").unwrap(),
    );
    registry.register(provider.clone());
    let dict = registry.build_dictionary();

    profile::extract_profiles(text, &mut ctx, Some(&dict));

    let chunks = chunk::plan(text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();

    let obs_cfg = provider.observation_config();
    let config = extract::Config {
        strong_verbs: obs_cfg.get(0).cloned().unwrap_or_default(),
        action_verbs: obs_cfg.get(2).cloned().unwrap_or_default(),
        ..extract::Config::default()
    };
    extract::compile(&mut ctx, &sent_texts, &dict, &config);

    let mut ft = faction::FactionTracker::from_file("三国演义", "config/faction_map.json").unwrap();
    for ev in &ctx.events {
        ft.process_event(ev);
    }

    let relations = timeline::build_timeline(&ctx.events, &[]);
    let entity_names: Vec<String> = ctx.entities.iter().map(|e| e.name.clone()).collect();
    let personality = timeline::extract_personality_markers(&ctx.events, Some(&doc.text), &entity_names);
    let arcs = timeline::build_character_arcs(personality.clone());

    let mut entity_events: HashMap<String, Vec<&Event>> = HashMap::new();
    for ev in &ctx.events {
        for p in &ev.participants {
            entity_events.entry(p.entity_name.clone()).or_default().push(ev);
        }
    }
    for v in entity_events.values_mut() {
        v.sort_by_key(|e| e.timestamp.unwrap_or(0));
    }

    let mut ranked: Vec<(&String, usize)> = entity_events.iter()
        .map(|(k, v)| (k, v.len())).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));

    let top_names: Vec<&str> = ranked.iter().take(15).map(|(n, _)| n.as_str()).collect();

    for name in &top_names {
        let faction_name = ft.faction_of(name).unwrap_or("未知");
        let baseline = ft.baseline_of(name).unwrap_or("未知");
        eprintln!("\n━━━ {} (阵营: {}, 基线: {}) ━━━━━", name, faction_name, baseline);

        let transitions: Vec<&faction::FactionTransition> = ft.transitions.iter()
            .filter(|t| t.entity == *name).collect();
        if !transitions.is_empty() {
            for t in transitions {
                eprintln!("      阵营变迁: Ch.{} {} → {} ({})", t.chapter, t.from_faction, t.to_faction, t.reason);
            }
        }

        let char_arc = arcs.iter().find(|a| a.entity == *name);
        if let Some(arc) = char_arc {
            if !arc.arc.is_empty() {
                eprintln!("      性格弧线: {}", arc.arc.join(" → "));
            }
        }

        if let Some(events) = entity_events.get(*name) {
            for ev in events.iter().take(10) {
                let ts = ev.timestamp.map(|t| format!("Ch.{}", t)).unwrap_or_default();
                let parts: Vec<&str> = ev.participants.iter()
                    .filter(|p| p.entity_name != *name)
                    .map(|p| p.entity_name.as_str())
                    .collect();
                let others = if parts.is_empty() { "".into() } else { format!(" ({})", parts.join(",")) };
                eprintln!("      {:>5}  {}  {}{}", ts, ev.event_type, ev.title, others);
            }
            if events.len() > 10 {
                eprintln!("      ... 还有 {} 件事件", events.len() - 10);
            }
        }

        let rels: Vec<&Relation> = relations.iter()
            .filter(|r| r.source == *name || r.target == *name).collect();
        if !rels.is_empty() {
            eprintln!("      关联人物:");
            for r in rels.iter().take(8) {
                let other = if r.source == *name { &r.target } else { &r.source };
                let from = r.valid_from.map(|t| format!("Ch.{}", t)).unwrap_or_default();
                let to = r.valid_to.map(|t| format!("→Ch.{}", t)).unwrap_or_default();
                eprintln!("        {} ──[{}]──> {} ({}{})", name, r.relation_type, other, from, to);
            }
            if rels.len() > 8 {
                eprintln!("        ... 还有 {} 条关系", rels.len() - 8);
            }
        }
    }

    eprintln!("\n\n━━━ 阵营关系图 ━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    let fg = ft.faction_graph(&ctx.entities);
    let mut fnames: Vec<&String> = fg.keys().collect();
    fnames.sort();
    for f in fnames {
        if let Some(members) = fg.get(f) {
            eprintln!("  {} ({}人): {}", f, members.len(), members.join("、"));
        }
    }

    eprintln!("\n\n═══ 统计 ═══");
    eprintln!("  实体: {}, 事件: {}, 关系: {}, 阵营变迁: {}, 性格标记:{}",
        ctx.entities.len(), ctx.events.len(), relations.len(),
        ft.transitions.len(), personality.len());

    assert!(ctx.events.len() > 100, "need events");
}
