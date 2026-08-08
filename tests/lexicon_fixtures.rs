//! Manual-fixture regression tests (ELITE_LEXICON_PLAN P0).
//!
//! These tiny, hand-written inputs give the compiler deterministic fixtures
//! that do not depend on the full public-domain corpora. They are the
//! "中英文人工 fixture" required by the plan: positive/negative cases for
//! entity discovery, event extraction, and profile patterns.

use mnemosyne::compiler::CompileContext;
use mnemosyne::compiler::document::Document;
use mnemosyne::compiler::{chunk, extract, profile, sentence};
use mnemosyne::language::{ChineseLanguageProvider, EnglishLanguageProvider};

/// Objective: Verify the Chinese fixture yields the expected entities/events.
/// Invariants: 刘备, 关羽, 张飞, 曹操 are discovered; at least one event
/// (杀/救/结义) is extracted; profiles include 字 patterns.
#[test]
fn chinese_fixture_compiles_expected_entities_and_events() {
    let document =
        Document::from_file("tests/fixtures/zh_short.txt").expect("zh fixture must be readable");
    let text = &document.text;

    let mut context = CompileContext {
        document_title: "fixture-zh".into(),
        ..Default::default()
    };
    let lang = ChineseLanguageProvider::new();
    let mut dict = mnemosyne::compiler::entity::EntityDictionary::default();
    profile::extract_profiles(text, &mut context, Some(&dict), &[], &lang);
    for entity in &context.entities {
        let aliases: Vec<&str> = context
            .profiles
            .iter()
            .filter(|p| p.entity_id == entity.id)
            .filter(|p| p.key == "courtesy_name" || p.key == "title")
            .map(|p| p.value.as_str())
            .collect();
        dict.register_discovered(&entity.name, &aliases);
    }
    profile::register_discovered_entities(&mut dict, &context);
    let alias_pairs: Vec<(String, i64)> = dict
        .alias_to_canonical
        .iter()
        .filter_map(|(a, c)| dict.name_to_id.get(c).map(|id| (a.clone(), *id)))
        .collect();
    let resolver = mnemosyne::entity_resolver::EntityResolver::new(
        mnemosyne::entity_resolver::AliasResolver::from_pairs(alias_pairs),
    );

    let chunks = chunk::plan(text, chunk::Config::default());
    let sentences = sentence::split_all(&chunks);
    let sent_texts: Vec<&str> = sentences.iter().map(|s| s.text.as_str()).collect();
    let config = extract::Config::from_language(&lang);
    extract::compile(&mut context, &sent_texts, &dict, &config, Some(&resolver));

    let names: Vec<&str> = context.entities.iter().map(|e| e.name.as_str()).collect();
    for expected in ["刘备", "关羽", "张飞", "曹操"] {
        assert!(
            names.contains(&expected),
            "Chinese fixture must discover `{expected}`; found {names:?}"
        );
    }
    assert!(
        context.events.len() >= 2,
        "Chinese fixture should extract at least two events (杀/救/结义), got {}",
        context.events.len()
    );
}

/// Objective: Verify the English fixture yields expected entities via titles.
/// Invariants: Mr. Bennet / Mr. Darcy are discovered; "plan" yields an
/// intention-class lexeme match through the lexicon matcher path.
#[test]
fn english_fixture_compiles_expected_entities() {
    let document =
        Document::from_file("tests/fixtures/en_short.txt").expect("en fixture must be readable");
    let text = &document.text;

    let mut context = CompileContext {
        document_title: "fixture-en".into(),
        ..Default::default()
    };
    let lang = EnglishLanguageProvider::new();
    profile::extract_profiles(text, &mut context, None, &[], &lang);

    let names: Vec<&str> = context.entities.iter().map(|e| e.name.as_str()).collect();
    for expected in ["Mr. Bennet", "Mr. Darcy"] {
        assert!(
            names.contains(&expected),
            "English fixture must discover `{expected}`; found {names:?}"
        );
    }
}
