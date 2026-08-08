//! End-to-end integration test: 三国演义 Observation → Fact → Snapshot → Context.
//! Run: cargo test --test cognition_e2e full_pipeline -- --nocapture

use mnemosyne::cognition::{FactStore, Rule, StateEngine, build_context, build_snapshot};
use mnemosyne::fact_store::SqliteFactStore;
use mnemosyne::language::{ChineseLanguageProvider, LanguageProvider};
use mnemosyne::observation_compiler::{DefaultRule, compile_observations};

const TEXT: &str = "刘备字玄德，涿郡人也。关羽字云长，张飞字翼德。桃园三结义，刘备、关羽、张飞结为兄弟。曹操字孟德，治世之能臣。吕布杀丁原。";

#[tokio::test]
async fn full_pipeline() {
    println!("========== Cognition Engine E2E ==========\n");

    // 1. Language Frontend
    let lang = ChineseLanguageProvider::new();
    println!("Frontend: {}", lang.name());

    // 2. Sentence splitting (by Chinese period)
    let sentences: Vec<&str> = TEXT.split('。').filter(|s| !s.is_empty()).collect();
    println!("Sentences: {}\n", sentences.len());

    // 3. Observation Compiler
    let verbs = vec![
        "杀".to_string(),
        "斩".to_string(),
        "擒".to_string(),
        "救".to_string(),
        "拜".to_string(),
        "结".to_string(),
        "曰".to_string(),
        "道".to_string(),
    ];

    // Dummy mention resolver — since we have no EntityResolver here,
    // we use a simple lookup table for the test
    let mention_map: std::collections::HashMap<&str, (i64, &str)> = [
        ("刘备", (10001, "刘备")),
        ("关羽", (10002, "关羽")),
        ("张飞", (10003, "张飞")),
        ("曹操", (10004, "曹操")),
        ("吕布", (10005, "吕布")),
        ("丁原", (10006, "丁原")),
    ]
    .iter()
    .copied()
    .collect();

    let resolve_mention = |text: &str| -> Option<mnemosyne::cognition::Mention> {
        for (key, (id, canonical)) in &mention_map {
            if text.contains(key) {
                return Some(mnemosyne::cognition::Mention {
                    entity_id: Some(*id),
                    surface: key.to_string(),
                    canonical_name: canonical.to_string(),
                });
            }
        }
        None
    };

    let observations = compile_observations(&sentences, &verbs, &resolve_mention);
    println!("Observations: {}", observations.len());
    for obs in &observations {
        println!("  {} -> {}", obs.subject.canonical_name, obs.action);
    }

    // 4. Rule → Facts
    let rule = DefaultRule;
    let mut facts = Vec::new();
    for obs in &observations {
        let mut result = rule.apply(obs);
        facts.append(&mut result);
    }
    println!("\nFacts: {}", facts.len());
    for fact in &facts {
        println!("  {:?} [{}] {}", fact.fact_type, fact.time, fact.payload);
    }

    // 5. FactStore
    let store = SqliteFactStore::open_in_memory().unwrap();
    let context_facts = facts.clone();
    let stored = store
        .insert_batch(&facts)
        .expect("The cognition fact batch should persist atomically");
    println!("\nStored: {} facts", stored);

    // 6. Verify stored facts
    for entity_id in &[10001i64, 10004, 10005] {
        let efacts = store
            .get_facts(*entity_id)
            .expect("Stored entity facts should remain readable");
        println!("Entity {}: {} facts", entity_id, efacts.len());
        for f in &efacts {
            println!("  {:?} [{}]", f.fact_type, f.time);
        }
    }

    // 7. State Engine + Snapshot
    let state_engine = StateEngine::new();
    // Test with 刘备's facts
    let liubei_facts = store
        .get_facts(10001)
        .expect("Liu Bei facts should remain readable for snapshot reconstruction");
    if !liubei_facts.is_empty() {
        let state = state_engine.aggregate(&liubei_facts);
        println!("\n刘备 State: {:?}", state);

        let snapshot = build_snapshot(
            10001,
            "刘备".into(),
            "person".into(),
            liubei_facts.clone(),
            &state_engine,
        );
        println!(
            "\n刘备 Snapshot (markdown):\n{}",
            snapshot.format_markdown()
        );
    }

    // 8. Cognitive Context
    let ctx = build_context(None, vec![], context_facts, (None, None));
    println!(
        "\nContext entities: {:?}",
        ctx.primary_entity.as_ref().map(|e| &e.entity_name)
    );

    // 9. Assertions
    assert!(
        observations.len() >= 2,
        "should extract at least 2 observations"
    );
    assert!(facts.len() >= 2, "should generate at least 2 facts");
    assert!(stored >= 2, "should store at least 2 facts");

    println!("\n========== E2E COMPLETE ==========");
}
