//! Full 三国演义 cognitive pipeline: Observation → Fact → Store → Snapshot.
//! Run: cargo test --test sanguo_cognition run -- --nocapture

use lore_scope::cognition::{FactStore, Rule, StateEngine};
use lore_scope::fact_store::SqliteFactStore;
use lore_scope::observation_compiler::{DefaultRule, compile_observations};

#[tokio::test]
async fn run() {
    println!("========== 三国演义 · Cognition Pipeline ==========\n");

    let text = std::fs::read_to_string("corpus/三国演义.txt").unwrap();
    println!("Text: {} chars\n", text.len());

    // 1. Sentence splitting (by Chinese period and other separators)
    let sentences: Vec<&str> = text
        .split(['。', '！', '？', '\n'])
        .filter(|s| s.len() >= 4)
        .collect();
    println!("Sentences: {}\n", sentences.len());

    // 2. Verbs for observation compiler
    let verbs: Vec<String> = vec![
        "杀".to_string(),
        "斩".to_string(),
        "擒".to_string(),
        "救".to_string(),
        "拜".to_string(),
        "结".to_string(),
        "曰".to_string(),
        "言".to_string(),
        "大怒".to_string(),
        "大喜".to_string(),
        "领兵".to_string(),
        "大战".to_string(),
        "笑".to_string(),
        "哭".to_string(),
        "骂".to_string(),
        "怒".to_string(),
        "封".to_string(),
        "赐".to_string(),
        "降".to_string(),
        "追".to_string(),
        "围".to_string(),
        "烧".to_string(),
        "逃".to_string(),
        "死".to_string(),
    ];

    // Built-in mention resolver uses alias matching from the dictionary
    // For now, use a simple lookup for key characters
    let known: std::collections::HashMap<&str, (i64, &str)> = [
        ("刘备", (10001, "刘备")),
        ("关羽", (10002, "关羽")),
        ("张飞", (10003, "张飞")),
        ("曹操", (10004, "曹操")),
        ("吕布", (10005, "吕布")),
        ("袁绍", (10006, "袁绍")),
        ("孙权", (10007, "孙权")),
        ("赵云", (10008, "赵云")),
        ("诸葛亮", (10009, "诸葛亮")),
        ("周瑜", (10010, "周瑜")),
        ("董卓", (10011, "董卓")),
        ("司马懿", (10012, "司马懿")),
        ("黄忠", (10013, "黄忠")),
        ("马超", (10014, "马超")),
        ("张辽", (10015, "张辽")),
    ]
    .iter()
    .copied()
    .collect();

    let resolve_mention = |text: &str| -> Option<lore_scope::cognition::Mention> {
        for (key, (id, canonical)) in &known {
            if text.contains(key) {
                return Some(lore_scope::cognition::Mention {
                    entity_id: Some(*id),
                    surface: key.to_string(),
                    canonical_name: canonical.to_string(),
                });
            }
        }
        None
    };

    // 3. Compile observations (sample: first 1000 sentences)
    let sample = &sentences[..sentences.len().min(1000)];
    let observations = compile_observations(sample, &verbs, &resolve_mention);
    println!("Observations: {}\n", observations.len());

    // Show sample observations
    for obs in observations.iter().take(10) {
        let sub = &obs.subject.canonical_name;
        let obj = obs
            .object
            .as_ref()
            .map(|o| o.canonical_name.as_str())
            .unwrap_or("?");
        println!("  {} -> {} -> {}", sub, obs.action, obj);
    }

    // 4. Facts from observations
    let rule = DefaultRule;
    let mut facts: Vec<lore_scope::cognition::Fact> = Vec::new();
    for obs in &observations {
        facts.append(&mut rule.apply(obs));
    }
    println!("\nFacts: {}", facts.len());
    // Show fact type distribution
    use std::collections::HashMap;
    let mut by_type: HashMap<String, usize> = HashMap::new();
    for f in &facts {
        let key = format!("{:?}", f.fact_type);
        *by_type.entry(key).or_default() += 1;
    }
    for (ft, count) in &by_type {
        println!("  {}: {}", ft, count);
    }

    // 5. Store facts
    let store = SqliteFactStore::open_in_memory().unwrap();
    let stored = store
        .insert_batch(&facts)
        .expect("The cognition fact batch should persist atomically");
    println!("\nStored: {} facts", stored);

    // 6. Entity snapshots
    let _state_engine = StateEngine::new();
    // Find entities with the most facts
    let mut by_entity: HashMap<i64, Vec<&lore_scope::cognition::Fact>> = HashMap::new();
    for f in &facts {
        by_entity.entry(f.entity_id).or_default().push(f);
    }
    let mut ranked: Vec<(&i64, &Vec<&lore_scope::cognition::Fact>)> = by_entity.iter().collect();
    ranked.sort_by_key(|b| std::cmp::Reverse(b.1.len()));

    println!("\n━━━ Top entities by fact count ━━━━━━━━━\n");
    for (eid, efacts) in ranked.iter().take(10) {
        println!("  Entity {}: {} facts", eid, efacts.len());
    }

    // 7. Stats
    println!("\n━━━ Stats ━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("  Sentences: {}", sample.len());
    println!("  Observations: {}", observations.len());
    println!("  Facts: {}", facts.len());
    println!("  Stored: {}", stored);
    println!("  Unique entities: {}", by_entity.len());

    assert!(!facts.is_empty(), "should generate facts");
    println!("\n========== COMPLETE ==========");
}
