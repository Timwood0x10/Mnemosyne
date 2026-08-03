//! War and Peace — try compiler pipeline (English novel).
//! Run: cargo test --test war_peace -- --nocapture

mod common;

use std::collections::HashMap;

#[tokio::test]
async fn war_peace() {
    println!("========== War and Peace · Analysis ==========\n");

    // Full 84k-sentence compile is ~2 minutes; replay the shared disk cache
    // (invalidated automatically when the corpus mtime changes).
    let compiled = common::ensure_war_compile();
    let text_len = std::fs::metadata("corpus/WarandPeace.txt")
        .map(|m| m.len())
        .unwrap_or(0);
    println!("Text: {} bytes\n", text_len);

    // Stats
    let mut counts: HashMap<String, usize> = HashMap::new();
    for ev in &compiled.events {
        for p in &ev.participants {
            *counts.entry(p.entity_name.clone()).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&String, &usize)> = counts.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1));

    println!("━━━ Events & entities ━━━━━━━━━━━━━━━━━━━\n");
    println!("Entities:  {}", compiled.entities.len());
    println!("Events:    {}", compiled.events.len());
    println!("Sentences: {} (cached compile)\n", common::WAR_CACHE_PATH);

    println!("Top 15 by event count:\n");
    for (name, count) in ranked.iter().take(15) {
        println!("  {:>5}  {}", count, name);
    }

    if !compiled.events.is_empty() {
        println!("\nSample events:\n");
        for ev in compiled.events.iter().take(10) {
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

    assert!(
        !compiled.entities.is_empty(),
        "English title discovery should produce at least one entity"
    );
    assert!(
        compiled
            .entities
            .iter()
            .all(|entity| entity.name.chars().any(char::is_alphabetic)),
        "Discovered English entities should contain alphabetic names"
    );
}
