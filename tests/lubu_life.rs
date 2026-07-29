//! Lü Bu's character growth trajectory — timeline, faction switches, personality arc.
//! Run: cargo test --test lubu_trajectory lubu_life -- --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::{CompileContext, Event, Relation};
use lore_scope::compiler::{chunk, extract, faction, profile, sentence, timeline};
use lore_scope::entity_resolver::{AliasResolver, EntityResolver};

#[tokio::test]
async fn lubu_life() {
    println!("╔══════════════════════════════════════════════╗");
    println!("║       Lü Bu (吕布) · Complete Life Arc       ║");
    println!("╚══════════════════════════════════════════════╝\n");

    let doc = Document::from_file("corpus/三国演义.txt").unwrap();
    let text = &doc.text;

    let mut ctx = CompileContext::default();
    ctx.document_title = "Romance of Three Kingdoms".into();

    let mut registry = EntityRegistry::new();
    let provider =
        Arc::new(JsonEntityProvider::from_file("config/entity_profiles/sanguo.json").unwrap());
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();

    profile::extract_profiles(text, &mut ctx, Some(&dict));
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

    let chunks = chunk::plan(text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();

    let obs_cfg = provider.observation_config();
    let config = extract::Config {
        strong_verbs: obs_cfg.first().cloned().unwrap_or_default(),
        action_verbs: obs_cfg.get(2).cloned().unwrap_or_default(),
        ..extract::Config::default()
    };
    extract::compile(
        &mut ctx,
        &sent_texts,
        &dict,
        &config,
        Some(&entity_resolver),
    );

    let entity_names: Vec<String> = ctx.entities.iter().map(|e| e.name.clone()).collect();
    let personality = timeline::extract_personality_markers(
        &ctx.events,
        Some(&doc.text),
        &entity_names,
        provider.personality_patterns(),
    );
    let arcs = timeline::build_character_arcs(personality);

    // ── 1. Profile ───────────────────────────────────────
    println!("━━━ 1. Entity Profile ━━━━━━━━━━━━━━━━━━━━━━━\n");
    if let Some(e) = ctx.entities.iter().find(|e| e.name == "吕布") {
        println!("  Name:     {}", e.name);
        println!("  Type:     {}", e.entity_type);
        println!("  Status:   {}", e.status);
        let pf: Vec<String> = ctx
            .profiles
            .iter()
            .filter(|p| p.entity_id == e.id)
            .map(|p| format!("{}: {}", p.key, p.value))
            .collect();
        for p in &pf {
            println!("  {}", p);
        }
    }

    // ── 2. Personality arc ───────────────────────────────
    println!("\n━━━ 2. Personality Arc ━━━━━━━━━━━━━━━━━━━━━\n");
    let arc = arcs.iter().find(|a| a.entity == "吕布");
    if let Some(a) = arc {
        let emoji = match a.arc_type.as_str() {
            "decline" => "📉 decline",
            "growth" => "📈 growth",
            "transformation" => "🔄 transformation",
            _ => "⏸ stable",
        };
        println!("  Arc type: {}", emoji);
        println!("  Traits:   {}", a.arc.join(" → "));
    } else {
        println!("  (No personality markers extracted for Lu Bu)");
    }

    // ── 3. Timeline ──────────────────────────────────────
    let lubu_events: Vec<&Event> = ctx
        .events
        .iter()
        .filter(|ev| {
            ev.title.contains("吕布")
                || ev.title.contains("奉先")
                || ev.participants.iter().any(|p| p.entity_name == "吕布")
        })
        .collect();

    println!(
        "\n━━━ 3. Life Timeline ({} events) ━━━━━━━\n",
        lubu_events.len()
    );

    let mut by_ts: HashMap<i32, Vec<&&Event>> = HashMap::new();
    for ev in &lubu_events {
        by_ts.entry(ev.timestamp.unwrap_or(0)).or_default().push(ev);
    }
    let mut tss: Vec<i32> = by_ts.keys().copied().collect();
    tss.sort();

    for ts in &tss {
        let marker = format!("第{}回", ts);
        let ctx_start = text
            .find(&marker)
            .map(|p| p.saturating_sub(20))
            .unwrap_or(0);
        let ctx_snip: String = text[ctx_start..].chars().take(100).collect();
        println!("  ═ Chapter {} ════════════════════", ts);
        println!(
            "  Context: {}",
            ctx_snip.lines().next().unwrap_or("").trim()
        );
        for ev in &by_ts[ts] {
            let parts: Vec<&str> = ev
                .participants
                .iter()
                .map(|p| p.entity_name.as_str())
                .collect();
            let pstr = if parts.is_empty() {
                "".into()
            } else {
                format!(" [{}]", parts.join(", "))
            };
            println!("    > {}{}", ev.title, pstr);
        }
        println!();
    }

    // ── 4. Relations ─────────────────────────────────────
    println!("━━━ 4. Relationship Network ━━━━━━━━━━━━━━━━━\n");
    let all_rels = timeline::build_timeline(&ctx.events, &[], &timeline::TimelineConfig::default());
    let rels: Vec<&Relation> = all_rels
        .iter()
        .filter(|r| r.source == "吕布" || r.target == "吕布")
        .collect();
    for r in &rels {
        let other = if r.source == "吕布" {
            &r.target
        } else {
            &r.source
        };
        let from = r
            .valid_from
            .map(|t| format!("Ch.{}", t))
            .unwrap_or_default();
        let to = r.valid_to.map(|t| format!("→Ch.{}", t)).unwrap_or_default();
        println!(
            "  吕布 ──[{}]──> {}  ({}{})",
            r.relation_type, other, from, to
        );
    }
    if rels.is_empty() {
        println!("  (No direct relations recorded)");
    }

    // ── 5. Stats summary ─────────────────────────────────
    println!("\n━━━ 5. Summary ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!(
        "  Active chapters: {} – {}",
        tss.first().unwrap_or(&0),
        tss.last().unwrap_or(&0)
    );
    println!("  Total events:    {}", lubu_events.len());
    let mut stats = entity_resolver.stats().clone();
    println!("  Resolver stats:");
    stats.print_report();
    println!("\n========== COMPLETE ==========");
}
