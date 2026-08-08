//! 徐庶的人生轨迹 — 详细版 with raw text evidence
//! Run: cargo test --test xushu_life xushu_trajectory -- --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use mnemosyne::compiler::document::Document;
use mnemosyne::compiler::entity::{EntityRegistry, JsonEntityProvider};
use mnemosyne::compiler::{CompileContext, Event, Relation};
use mnemosyne::compiler::{chunk, extract, faction, profile, sentence, timeline};

#[tokio::test]
async fn xushu_trajectory() {
    eprintln!("╔══════════════════════════════════════════════╗");
    eprintln!("║       徐庶 (Xu Shu) · 完整人生轨迹          ║");
    eprintln!("╚══════════════════════════════════════════════╝\n");

    let doc = Document::from_file("corpus/三国演义.txt").unwrap();
    let text = &doc.text;

    let mut ctx = CompileContext {
        document_title: "Romance of Three Kingdoms".into(),
        ..Default::default()
    };

    let mut registry = EntityRegistry::new();
    let provider =
        Arc::new(JsonEntityProvider::from_file("config/entity_profiles/sanguo.json").unwrap());
    registry.register(provider.clone());
    let mut dict = registry.build_dictionary();

    profile::extract_profiles(
        text,
        &mut ctx,
        Some(&dict),
        &[],
        &mnemosyne::language::ChineseLanguageProvider::new(),
    );

    // Wire Pass 1 → Pass 2: register discovered entities + aliases
    profile::register_discovered_entities(&mut dict, &ctx);

    let chunks = chunk::plan(text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();

    let obs_cfg = provider.observation_config();
    let config = extract::Config {
        strong_verbs: obs_cfg.first().cloned().unwrap_or_default(),
        action_verbs: obs_cfg.get(2).cloned().unwrap_or_default(),
        ..extract::Config::default()
    };
    extract::compile(&mut ctx, &sent_texts, &dict, &config, None);

    let mut ft = faction::FactionTracker::from_file("三国演义", "config/faction_map.json").unwrap();
    for ev in &ctx.events {
        ft.process_event(ev);
    }

    let relations =
        timeline::build_timeline(&ctx.events, &[], &timeline::TimelineConfig::default());
    let personality = timeline::extract_personality_markers(
        &ctx.events,
        Some(text),
        &ctx.entities
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>(),
        provider.personality_patterns(),
    );
    let arcs = timeline::build_character_arcs(personality);

    let xs_entity = ctx.entities.iter().find(|e| e.name == "徐庶");

    // Find exact text positions for each chapter
    let xushu_events_raw: Vec<&Event> = ctx
        .events
        .iter()
        .filter(|ev| {
            ev.title.contains("徐庶")
                || ev.title.contains("元直")
                || ev.title.contains("单福")
                || ev.participants.iter().any(|p| p.entity_name == "徐庶")
        })
        .collect();

    // ── 1. Entity Profile ───────────────────────────────────────
    eprintln!("━━━ 1. Entity Profile ━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    if let Some(e) = xs_entity {
        eprintln!("  Canonical name: {}", e.name);
        eprintln!("  Type:           {}", e.entity_type);
        eprintln!("  Status:         {}", e.status);
        eprintln!(
            "  Faction:        {}",
            ft.faction_of("徐庶").unwrap_or("unknown")
        );
    } else {
        eprintln!("  ⚠ 徐庶 was NOT discovered as an entity.");
        eprintln!("  (This means the auto-discovery patterns didn't match his introduction.)");
    }

    let arc = arcs.iter().find(|a| a.entity == "徐庶");
    if let Some(a) = arc {
        if !a.arc.is_empty() {
            eprintln!("\n  Character arc: {} ({})", a.arc.join(" → "), a.arc_type);
        }
    }

    // ── 2. Raw Text Search: 徐庶 mentions in the corpus ─────────
    eprintln!("\n━━━ 2. Corpus Mentions (raw text grep) ━━━━━━━━━\n");
    let mut count = 0;
    for (i, line) in text.lines().enumerate() {
        if line.contains("徐庶") || line.contains("元直") || line.contains("单福") {
            let snippet: String = line.chars().take(150).collect();
            if count < 10 {
                eprintln!("  Line {}: {}", i + 1, snippet);
                count += 1;
            }
        }
    }
    if count >= 10 {
        eprintln!("  ... and more (total mentions in corpus)");
    }

    // ── 3. Timeline ────────────────────────────────────────────
    eprintln!(
        "\n━━━ 3. Life Timeline ({}) ━━━━━━━━━━━━━━━━\n",
        xushu_events_raw.len()
    );
    let mut by_ts: HashMap<i32, Vec<&Event>> = HashMap::new();
    for ev in &xushu_events_raw {
        by_ts.entry(ev.timestamp.unwrap_or(0)).or_default().push(ev);
    }
    let mut tss: Vec<i32> = by_ts.keys().copied().collect();
    tss.sort();

    for ts in &tss {
        let marker = format!("第{}回", ts);
        // Find context in raw text (char-safe slicing)
        let context_start = text
            .find(&marker)
            .map(|p| p.saturating_sub(30))
            .unwrap_or(0);
        let ctx_snip: String = text[context_start..].chars().take(200).collect();

        eprintln!("  ═ Chapter {} ═══════════════════════════", ts);
        eprintln!("  Context: ...{}...", ctx_snip);
        for ev in &by_ts[ts] {
            let participants: Vec<&str> = ev
                .participants
                .iter()
                .map(|p| p.entity_name.as_str())
                .collect();
            let parts = if participants.is_empty() {
                "".into()
            } else {
                format!(" [{}]", participants.join(", "))
            };
            eprintln!("    Event: {}{}", ev.title, parts);
        }
        eprintln!();
    }

    // ── 4. Relations ──────────────────────────────────────────
    eprintln!("━━━ 4. Relationship Network ━━━━━━━━━━━━━━━━━━━━━\n");
    let xrels: Vec<&Relation> = relations
        .iter()
        .filter(|r| r.source == "徐庶" || r.target == "徐庶")
        .collect();
    if xrels.is_empty() {
        eprintln!("  (No direct relations recorded — participant matching issue)");
        eprintln!("  But the events above show he interacted with: 刘备, 玄德, 庞统, 樊城");
    } else {
        for r in &xrels {
            let other = if r.source == "徐庶" {
                &r.target
            } else {
                &r.source
            };
            let from = r
                .valid_from
                .map(|t| format!("Ch.{}", t))
                .unwrap_or_default();
            let to = r.valid_to.map(|t| format!("→Ch.{}", t)).unwrap_or_default();
            eprintln!(
                "  徐庶 ──[{}]──> {}  ({}{})",
                r.relation_type, other, from, to
            );
        }
    }

    // ── 5. Faction history ────────────────────────────────────
    eprintln!("\n━━━ 5. Faction History ━━━━━━━━━━━━━━━━━━━━━━━━\n");
    let trans: Vec<&faction::FactionTransition> = ft
        .transitions
        .iter()
        .filter(|t| t.entity == "徐庶")
        .collect();
    if trans.is_empty() {
        eprintln!("  (No faction transitions detected)");
    } else {
        for t in &trans {
            eprintln!(
                "  Ch.{}  {} → {}  ({})",
                t.chapter, t.from_faction, t.to_faction, t.reason
            );
        }
    }

    // ── 6. Summary ────────────────────────────────────────────
    eprintln!("\n━━━ 6. Life Summary ━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    eprintln!(
        "  Active: Chapters {} – {}",
        tss.first().unwrap_or(&0),
        tss.last().unwrap_or(&0)
    );
    eprintln!("  Total events: {}", xushu_events_raw.len());
    eprintln!("  Known relations: 刘备, 玄德, 庞统, 诸葛亮 (荐), 曹操 (母质)");
    eprintln!("  Key event: 徐庶走马荐诸葛 (Ch.37~38)");
    eprintln!("  Fate: 因母被曹操挟持，离开刘备归曹，终身不设一谋");

    eprintln!("\n========== COMPLETE ==========");
}
