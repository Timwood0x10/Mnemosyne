//! End-to-end demo: compile 西游记 (three-rhino chapter) and query via KnowledgeStore.
//!
//! Run: cargo test --test lore_compile_integration rhino_demo -- --nocapture

use std::sync::Arc;

use lore_scope::compiler::chunk;
use lore_scope::compiler::document::Document;
use lore_scope::compiler::entity::{EntityEngine, EntityRegistry, JsonEntityProvider};
use lore_scope::compiler::pronoun::PronounResolver;
use lore_scope::compiler::sentence;
use lore_scope::compiler::{builder, inference, merge, observation};
use lore_scope::compiler::writer;
use lore_scope::knowledge::store::KnowledgeStore;
use lore_scope::knowledge::SQLiteKnowledgeStore;

/// Excerpt from 西游记 chapter 92: the three rhino spirits' fate.
const XIYOU_RHINO: &str = "
第九十二回 三僧大战青龙山 四星挟捉犀牛怪

却说那三个妖魔：辟寒大王、辟暑大王、辟尘大王，原是犀牛之精。
井木犴现原身按住辟寒儿大口小口的啃着吃哩。
摩昂高叫道：井宿！井宿！莫咬死他，孙大圣要活的不要死的哩。
连喊数喊，已是被他把颈项咬断了。
行者道：取锯子来锯下他的这两只角，剥了皮带去。

又把辟尘儿穿了鼻教角木蛟牵着，辟暑儿也穿了鼻教井木犴牵着。
带他上金平府见那刺史官，明究其由，问他个积年假佛害民，然后的决。
";

#[tokio::test]
async fn rhino_demo() {
    eprintln!("\n=== 西游记 犀牛精 编译与查询 ===\n");

    let doc = Document::from_text("西游记", "novel", XIYOU_RHINO);
    let store = Arc::new(
        SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("open store"),
    );

    // ---- Pipeline ----
    let chunks = chunk::plan(&doc.text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    eprintln!("Sentences: {}", sentences.len());

    let mut registry = EntityRegistry::new();
    let provider = Arc::new(
        JsonEntityProvider::from_file("config/entity_profiles/xiyou.json")
            .expect("load xiyou profile"),
    );
    let obs_config = provider.observation_config();
    registry.register(provider);
    let engine = EntityEngine::new(registry);
    let mentions = engine.scan_sentences(&sentences);
    eprintln!("Mentions:  {}", mentions.len());

    let pronoun = PronounResolver::new();
    let resolved = pronoun.resolve(&mentions);

    let mut observations = Vec::new();
    for sent in &sentences {
        observations.extend(observation::extract_observations(sent, &resolved, &obs_config));
    }
    eprintln!("Observations: {}", observations.len());

    let result = builder::build(&observations);
    let merged = merge::merge(vec![result]);
    let rule_engine = inference::RuleEngine::new();
    let final_result = rule_engine.apply(&merged);
    writer::write(&final_result, &doc.title, &*store)
        .await
        .expect("write");

    // ---- Query ----
    for name in &["辟寒大王", "辟暑大王", "辟尘大王", "井木犴"] {
        match store.inspect_entity(name, Some("西游记")).await.unwrap() {
            Some(entity) => {
                eprintln!("\n  ✓ {}: object_type={:?}, relations={}, evidence={}",
                    name, entity.object.object_type, entity.relations.len(), entity.evidences.len());
            }
            None => eprintln!("\n  ⚠ {}: not found in knowledge graph", name),
        }
    }

    // Search evidence for "角" (horns)
    let hits = store.search_evidence("锯下", Some("西游记"), 10).await.unwrap();
    eprintln!("\n证据搜索 \"锯下\": {} 条结果", hits.len());
    for h in &hits {
        eprintln!("  → {}", h.text);
    }

    let hits2 = store.search_evidence("角", Some("西游记"), 10).await.unwrap();
    eprintln!("\n证据搜索 \"角\": {} 条结果", hits2.len());
    for h in &hits2 {
        eprintln!("  → {}", h.text);
    }
}
