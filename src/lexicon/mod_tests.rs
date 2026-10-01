//! Unit tests for the lexicon registry and layered build.
//!
//! Extracted from `mod.rs` via `#[path]` so the module source stays under the
//! 1000-line limit (`plan/rules/rules.md` §1). The tests reach the parent's
//! private items through `use super::*`.

use super::*;

/// Objective: Load core lexicon from the real config file.
/// Invariants: At least 50 lexemes are loaded (core EN + ZH minimal set).
#[test]
fn core_lexicon_loads_and_contains_entries() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core lexicon must load")
        .build()
        .expect("Registry build must succeed");
    assert!(
        registry.lexemes().len() >= 50,
        "Core lexicon should contain at least 50 lexemes, got {}",
        registry.lexemes().len()
    );
    assert!(
        !registry.content_hash().is_empty(),
        "Content hash must be non-empty"
    );
}

/// Objective: Verify the matcher build counter increments on construction.
/// Invariants: Building a matcher increases `matcher_build_count()`; the
/// global functional matcher (LazyLock) is built exactly once per process.
#[test]
fn matcher_build_count_tracks_constructions() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .build()
        .expect("Registry build must succeed");

    let before = matcher_build_count();
    let _m1 = LexiconMatcher::from_lexemes(registry.lexemes());
    let _m2 = LexiconMatcher::from_lexemes(registry.lexemes());
    let after = matcher_build_count();
    // The counter is process-global and other tests build matchers in
    // parallel, so only an at-least assertion is deterministic here.
    assert!(
        after - before >= 2,
        "Two explicit matcher constructions must bump the counter by at least 2 (got {})",
        after - before
    );

    // Global matcher is lazily built once; force it and ensure the count
    // does not explode when reused (it is a LazyLock singleton).
    let _g = crate::lexicon::LexiconMatcher::from_global_registry();
    let _g2 = crate::lexicon::LexiconMatcher::from_global_registry();
    assert!(
        matcher_build_count() >= after,
        "Global matcher construction must also be counted"
    );
}

/// Objective: Verify duplicate IDs are rejected.
/// Invariants: Two lexemes with the same ID in the same layer produce an error.
#[test]
fn duplicate_id_is_detected() {
    let lexemes = vec![
        make_test_lexeme("en.test.dup", "test_a"),
        make_test_lexeme("en.test.dup", "test_b"),
    ];
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    // This should fail because we have dup IDs in the user layer
    let result = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_user(lexemes)
        .build();
    assert!(
        result.is_err(),
        "Duplicate IDs in the same layer should be rejected"
    );
}

/// Objective: Verify duplicate forms (same language + same lemma) are rejected.
/// Invariants: Same lemma+language in the same layer produces an error.
#[test]
fn duplicate_form_is_detected() {
    let lexemes = vec![
        Lexeme {
            id: "en.test.first".into(),
            language: "en".into(),
            lemma: "testword".into(),
            forms: vec![],
            pos: "verb".into(),
            semantic_class: "speech".into(),
            effects: vec![],
            polarity: "neutral".into(),
            priority: 500,
            constraints: crate::dictionary::MatchConstraints {
                word_boundary: true,
                allow_single: false,
                requires_participant: false,
                requires_subject: false,
            },
            source: crate::dictionary::LexiconSource {
                kind: "builtin".into(),
                name: "test".into(),
            },
            status: crate::dictionary::LexemeStatus::Core,
        },
        Lexeme {
            id: "en.test.second".into(),
            language: "en".into(),
            lemma: "testword".into(),
            forms: vec![],
            pos: "verb".into(),
            semantic_class: "speech".into(),
            effects: vec![],
            polarity: "neutral".into(),
            priority: 500,
            constraints: crate::dictionary::MatchConstraints {
                word_boundary: true,
                allow_single: false,
                requires_participant: false,
                requires_subject: false,
            },
            source: crate::dictionary::LexiconSource {
                kind: "builtin".into(),
                name: "test".into(),
            },
            status: crate::dictionary::LexemeStatus::Core,
        },
    ];
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let result = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_user(lexemes)
        .build();
    assert!(
        result.is_err(),
        "Duplicate forms (same lemma+language) in the same layer should be rejected"
    );
}

/// Objective: Verify the global singleton initializes without panic.
/// Invariants: Calling `global()` returns a valid registry.
#[test]
fn global_registry_initializes() {
    let r = global();
    assert!(
        r.lexemes().len() >= 50,
        "Global registry should have >= 50 lexemes"
    );
}

/// Objective: Verify a domain pack merges on top of the core lexicon.
/// Invariants: The merged registry contains conversation lexemes from the
/// pack in addition to the core set, without duplicate-ID errors.
#[test]
fn domain_pack_merges_with_core() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let pack_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/conversation_memory.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_domain(&pack_path)
        .expect("conversation_memory pack must load")
        .build()
        .expect("Registry build must succeed");

    // The pack adds "喜欢" (zh preference) and "want" (en goal).
    assert!(
        registry
            .by_id("zh.conversation.preference.xihuan")
            .is_some(),
        "Domain pack lexeme 喜欢 must be present"
    );
    assert!(
        registry.by_id("en.conversation.goal.want").is_some(),
        "Domain pack lexeme want must be present"
    );
    assert!(
        registry.by_id("en.action.speech.said").is_some(),
        "Core lexeme must still be present after merge"
    );
}

/// Objective: Verify the english_narrative domain pack merges on top of core.
/// Invariants: Narrative lexemes (e.g. murmured, invaded) are present;
/// core entries remain; no cross-layer duplicate-ID errors.
#[test]
fn english_narrative_pack_merges_with_core() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let pack_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/english_narrative.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_domain(&pack_path)
        .expect("english_narrative pack must load")
        .build()
        .expect("Registry build must succeed");

    assert!(
        registry.by_id("en.narrative.speech.murmured").is_some(),
        "Narrative lexeme murmured must be present"
    );
    assert!(
        registry.by_id("en.narrative.attack.invaded").is_some(),
        "Narrative lexeme invaded must be present"
    );
    assert!(
        registry.by_id("en.action.speech.said").is_some(),
        "Core lexeme said must still be present after merge"
    );
}

/// Objective: Verify the sanguo work-specific pack merges and is isolated.
/// Invariants: Work-specific lexemes (青龙偃月刀, 结义) come from the pack,
/// not from Core; core remains free of work-specific terms.
#[test]
fn sanguo_pack_merges_and_core_stays_clean() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let pack_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/sanguo.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_domain(&pack_path)
        .expect("sanguo pack must load")
        .build()
        .expect("Registry build must succeed");

    assert!(
        registry.by_id("zh.sanguo.weapon.qinglong").is_some(),
        "Work-specific lexeme 青龙偃月刀 must be present via the pack"
    );
    assert!(
        registry.by_id("zh.sanguo.event.jieyi").is_some(),
        "Work-specific lexeme 结义 must be present via the pack"
    );

    // P4 isolation guard: Core alone must NOT contain work-specific terms.
    let core_only = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .build()
        .expect("Core-only build must succeed");
    for work_specific in ["青龙偃月刀", "丈八蛇矛", "结义", "出茅庐", "奸雄"] {
        assert!(
            core_only.lookup(work_specific).is_empty(),
            "Core must not contain work-specific lexeme `{work_specific}`"
        );
    }
}
/// Invariants: Selecting only `conversation_memory` excludes classical
/// lexemes while keeping core + the selected pack.
/// Objective: Verify a per-request pack selection limits which packs contribute entries.
/// Invariants: only the selected pack's entries are present and the others are absent.
#[test]
fn per_request_pack_selection_filters_packs() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let conversation =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/conversation_memory.json");
    let classical =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/classical_chinese.json");

    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_domain_pack("conversation_memory", &conversation)
        .expect("conversation pack must load")
        .load_domain_pack("classical_chinese", &classical)
        .expect("classical pack must load")
        .select_packs(&["conversation_memory"])
        .build()
        .expect("Registry build must succeed");

    assert!(
        registry
            .by_id("zh.conversation.preference.xihuan")
            .is_some(),
        "Selected pack lexeme must be present"
    );
    assert!(
        registry.by_id("zh.classical.attack.fa").is_none(),
        "Unselected pack lexeme must be excluded"
    );
    assert!(
        registry.by_id("en.action.speech.said").is_some(),
        "Core lexeme must always be present"
    );
}

/// Objective: Verify cross-pack duplicate IDs are diagnosed.
/// Invariants: Two packs defining the same lexeme ID produce a
/// `CrossPackDuplicateId` error at build time.
#[test]
fn cross_pack_duplicate_id_is_detected() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let dir = tempfile::TempDir::new().expect("temp dir for cross-pack test");
    let pack_a_path = dir.path().join("pack_a.json");
    let pack_b_path = dir.path().join("pack_b.json");

    let lexeme_a = make_test_lexeme("en.test.shared", "shared_a");
    let lexeme_b = make_test_lexeme("en.test.shared", "shared_b");
    std::fs::write(
        &pack_a_path,
        serde_json::to_string(&vec![lexeme_a]).expect("serialize pack_a"),
    )
    .expect("write pack_a");
    std::fs::write(
        &pack_b_path,
        serde_json::to_string(&vec![lexeme_b]).expect("serialize pack_b"),
    )
    .expect("write pack_b");

    let err = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_domain_pack("pack_a", &pack_a_path)
        .expect("pack_a must load")
        .load_domain_pack("pack_b", &pack_b_path)
        .expect("pack_b must load")
        .build()
        .expect_err("cross-pack duplicate ID must fail");

    match err {
        LexiconError::CrossPackDuplicateId { id, pack_a, pack_b } => {
            assert_eq!(id, "en.test.shared", "conflicting ID must be reported");
            assert_eq!(pack_a, "pack_a", "first pack name must be reported");
            assert_eq!(pack_b, "pack_b", "second pack name must be reported");
        }
        other => panic!("expected CrossPackDuplicateId, got {other:?}"),
    }
}

/// Objective: Verify the manifest reports entry counts and status lists.
/// Invariants: Manifest has the same total as `lexemes()`; en/zh counts sum
/// to the total; with core+classical packs the counts are non-zero.
#[test]
fn manifest_reports_counts_and_statuses() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let pack_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/classical_chinese.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_domain(&pack_path)
        .expect("Pack must load")
        .build()
        .expect("Registry build must succeed");

    let manifest = registry.manifest();
    assert_eq!(
        manifest.total_entries,
        registry.lexemes().len(),
        "Manifest total must match registry lexeme count"
    );
    assert_eq!(
        manifest.en_entries + manifest.zh_entries,
        manifest.total_entries,
        "en+zh counts must sum to the total"
    );
    assert!(manifest.en_entries > 0, "English entries must exist");
    assert!(manifest.zh_entries > 0, "Chinese entries must exist");
    assert!(
        !manifest.content_hash.is_empty(),
        "Manifest must carry the content hash"
    );
}

/// Objective: Verify the manifest serializes to stable JSON (P6 inventory).
/// Invariants: `to_json()` output parses back and contains the hash and
/// entry counts; the output is deterministic.
#[test]
fn manifest_serializes_to_json() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .build()
        .expect("Registry build must succeed");

    let json = registry.manifest().to_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("manifest JSON must parse");
    assert!(
        parsed["content_hash"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "manifest JSON must carry the content hash"
    );
    assert!(
        parsed["total_entries"].as_u64().is_some(),
        "manifest JSON must carry total_entries"
    );
    assert_eq!(
        registry.manifest().to_json(),
        json,
        "manifest JSON must be deterministic"
    );
}

/// Objective: Verify deprecated/disabled ID reporting.
/// Invariants: A Disabled user lexeme appears in `disabled_ids()` but not
/// in matchers; a Core lexeme is not reported.
#[test]
fn manifest_lists_disabled_and_deprecated() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let mut disabled_lex = make_test_lexeme("en.test.to_disable", "tobedisabled");
    disabled_lex.status = crate::dictionary::LexemeStatus::Disabled;
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_user(vec![disabled_lex])
        .build()
        .expect("Registry build must succeed");

    assert!(
        registry
            .disabled_ids()
            .contains(&"en.test.to_disable".to_string()),
        "Disabled lexeme must be reported by disabled_ids()"
    );
    assert!(
        !registry
            .deprecated_ids()
            .contains(&"en.test.to_disable".to_string()),
        "Disabled lexeme must not be reported as deprecated"
    );
    assert!(
        registry.by_id("en.action.speech.said").is_some(),
        "Core lexeme must remain present"
    );
}

/// Objective: Verify the classical_chinese domain pack merges on top of core.
/// Invariants: Classical lexemes (e.g. 伐, 弑) are present; core entries
/// remain; duplicate-ID validation does not trip across layers.
#[test]
fn classical_chinese_pack_merges_with_core() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let pack_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/classical_chinese.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_domain(&pack_path)
        .expect("classical_chinese pack must load")
        .build()
        .expect("Registry build must succeed");

    assert!(
        registry.by_id("zh.classical.attack.fa").is_some(),
        "Classical lexeme 伐 must be present"
    );
    assert!(
        registry.by_id("zh.classical.attack.shishi").is_some(),
        "Classical lexeme 弑 must be present"
    );
    assert!(
        registry.by_id("zh.action.attack.sha").is_some(),
        "Core lexeme 杀 must still be present after merge"
    );
}

/// Objective: Verify hit metrics accumulate and snapshot deterministically.
/// Invariants: Total hits and per-id/class counts reflect recorded hits;
/// the snapshot is sorted so repeated snapshots are identical.
#[test]
fn metrics_record_and_snapshot() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .build()
        .expect("Registry build must succeed");

    registry.record_hit("en.action.speech.said", "speech");
    registry.record_hit("en.action.speech.said", "speech");
    registry.record_hit("zh.action.attack.sha", "attack");
    registry.record_rejection();

    let snap = registry.metrics();
    assert_eq!(snap.total_hits, 3, "Three hits must be recorded");
    assert_eq!(snap.total_rejections, 1, "One rejection must be recorded");
    assert_eq!(
        snap.hits_by_id,
        vec![
            ("en.action.speech.said".to_string(), 2),
            ("zh.action.attack.sha".to_string(), 1),
        ],
        "Per-id counts must be deterministic and sorted"
    );
    assert_eq!(
        snap.hits_by_class,
        vec![("attack".to_string(), 1), ("speech".to_string(), 2),],
        "Per-class counts must be deterministic and sorted"
    );
    // Snapshot twice: must be identical (deterministic).
    assert_eq!(snap, registry.metrics(), "Snapshots must be deterministic");
}

/// Objective: Verify the matcher finds lexeme forms with word boundaries.
/// Invariants: "plan" matches but "planet" does not (word boundary); the
/// matcher reports the correct lexeme ID and semantic class.
#[test]
fn matcher_honors_word_boundaries() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let pack_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/conversation_memory.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .load_domain(&pack_path)
        .expect("Pack must load")
        .build()
        .expect("Registry build must succeed");

    let matcher = LexiconMatcher::from_lexemes(registry.lexemes());
    assert!(matcher.pattern_count() > 0, "Matcher must contain patterns");

    // "I plan to rewrite the parser." — "plan" is a goal lexeme.
    let text = "I plan to rewrite the parser.";
    let matches: Vec<LexiconMatch> = matcher.find_iter(text).collect();
    assert!(
        matches.iter().any(|m| m.semantic_class == "intention"),
        "`plan` should match an intention lexeme; got {matches:?}"
    );

    // "planet" must NOT match the `plan` pattern (word boundary).
    let planet_matches: Vec<LexiconMatch> = matcher.find_iter("explore the planet").collect();
    assert!(
        !planet_matches.iter().any(|m| m.matched == "plan"),
        "`plan` inside `planet` must be rejected by word boundary; got {planet_matches:?}"
    );
}

/// Objective: Verify Chinese single-character matching works.
/// Invariants: "杀" in 三国演义 text matches an attack lexeme.
#[test]
fn matcher_finds_chinese_single_char() {
    let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
    let registry = RegistryBuilder::new()
        .load_core(&core_path)
        .expect("Core must load")
        .build()
        .expect("Registry build must succeed");

    let matcher = LexiconMatcher::from_lexemes(registry.lexemes());
    let matches: Vec<LexiconMatch> = matcher.find_iter("吕布杀董卓").collect();
    assert!(
        matches
            .iter()
            .any(|m| m.matched == "杀" && m.semantic_class == "attack"),
        "`杀` should match an attack lexeme; got {matches:?}"
    );
}

fn make_test_lexeme(id: &str, lemma: &str) -> Lexeme {
    Lexeme {
        id: id.into(),
        language: "en".into(),
        lemma: lemma.into(),
        forms: vec![],
        pos: "verb".into(),
        semantic_class: "speech".into(),
        effects: vec![],
        polarity: "neutral".into(),
        priority: 500,
        constraints: crate::dictionary::MatchConstraints {
            word_boundary: true,
            allow_single: false,
            requires_participant: false,
            requires_subject: false,
        },
        source: crate::dictionary::LexiconSource {
            kind: "builtin".into(),
            name: "test".into(),
        },
        status: crate::dictionary::LexemeStatus::Core,
    }
}

// ── Layered build (D4) and fail-loud load (D5) ─────────────────────

/// A minimal attack lexeme JSON object with the given id/lemma/status.
fn fixture_lexeme(id: &str, lemma: &str, status: &str) -> String {
    format!(
        r#"{{"id":"{id}","language":"en","lemma":"{lemma}","pos":"verb","semantic_class":"attack","effects":[],"polarity":"neutral","priority":500,"constraints":{{"word_boundary":true,"allow_single":false,"requires_participant":false,"requires_subject":false}},"source":{{"kind":"builtin","name":"test"}},"status":"{status}"}}"#
    )
}

/// Write a fixture file, creating its parent directories.
fn write_fixture(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture dir");
    }
    std::fs::write(path, contents).expect("write fixture file");
}

/// Objective: Verify the production layered build actually applies the
/// documented precedence (core → domain packs → user override). Before the
/// fix only `load_core` ran in production, so packs and overrides did
/// nothing (audit H17).
/// Invariants: an ID defined in all three layers resolves to the user
/// version, then the pack version once the user layer is gone, then the
/// core version; a core-only lexeme survives every overlay.
#[test]
fn layered_build_applies_core_pack_user_precedence() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path();
    let conflict = "en.test.conflict";
    write_fixture(
        &root.join("config/dictionary.json"),
        &format!(
            r#"{{"lexemes":[{},{}]}}"#,
            fixture_lexeme(conflict, "coreword", "core"),
            fixture_lexeme("en.test.coreonly", "coreonlyword", "core")
        ),
    );
    write_fixture(
        &root.join("lexicon/packs/pack_a.json"),
        &format!("[{}]", fixture_lexeme(conflict, "packword", "core")),
    );
    write_fixture(
        &root.join("lexicon/user.json"),
        &format!("[{}]", fixture_lexeme(conflict, "userword", "core")),
    );

    // All three layers present: the user override wins.
    let reg = build_registry_at(root).expect("layered build must succeed");
    assert_eq!(
        reg.by_id(conflict).expect("conflict lexeme present").lemma,
        "userword",
        "the user override must win over core and the domain pack"
    );
    assert!(
        reg.by_id("en.test.coreonly").is_some(),
        "a core-only lexeme must survive the overlays"
    );

    // Without the user layer, the pack wins over core.
    std::fs::remove_file(root.join("lexicon/user.json")).expect("remove user fixture");
    let reg = build_registry_at(root).expect("build without the user layer");
    assert_eq!(
        reg.by_id(conflict).expect("conflict lexeme present").lemma,
        "packword",
        "the domain pack must win over core when no user override exists"
    );

    // With neither overlay, core is the last one standing.
    std::fs::remove_file(root.join("lexicon/packs/pack_a.json")).expect("remove pack fixture");
    let reg = build_registry_at(root).expect("core-only build");
    assert_eq!(
        reg.by_id(conflict).expect("conflict lexeme present").lemma,
        "coreword",
        "core must be the fallback when no overlay defines the id"
    );
}

/// Objective: Verify `lexicon/packs/*.json` is discovered and merged, in a
/// deterministic order, so dropping a pack file in takes effect (audit H17).
/// Invariants: lexemes from both packs are present alongside core.
#[test]
fn packs_are_discovered_from_the_packs_directory() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path();
    write_fixture(
        &root.join("config/dictionary.json"),
        &format!(
            r#"{{"lexemes":[{}]}}"#,
            fixture_lexeme("en.test.coreonly", "coreonlyword", "core")
        ),
    );
    write_fixture(
        &root.join("lexicon/packs/zeta.json"),
        &format!("[{}]", fixture_lexeme("en.test.zeta", "zetaword", "core")),
    );
    write_fixture(
        &root.join("lexicon/packs/alpha.json"),
        &format!("[{}]", fixture_lexeme("en.test.alpha", "alphaword", "core")),
    );

    let reg = build_registry_at(root).expect("build with two packs");
    assert!(
        reg.by_id("en.test.alpha").is_some(),
        "the alpha pack lexeme must be merged"
    );
    assert!(
        reg.by_id("en.test.zeta").is_some(),
        "the zeta pack lexeme must be merged"
    );
    assert!(
        reg.by_id("en.test.coreonly").is_some(),
        "core must remain present alongside the packs"
    );
}

/// Objective: Verify a `Disabled` lexeme is genuinely excluded from the
/// production registry and therefore from the matcher. `LexemeStatus::
/// Disabled` claims "excluded from matchers entirely", but nothing acted on
/// it — the only user-facing way to switch a shipped word off (audit H17).
/// Invariants: the disabled id is absent from `by_id`/`lexemes()` and the
/// matcher never reports it, while the active lexeme still matches.
#[test]
fn disabled_lexeme_is_excluded_from_the_registry_and_matcher() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path();
    write_fixture(
        &root.join("config/dictionary.json"),
        &format!(
            r#"{{"lexemes":[{},{}]}}"#,
            fixture_lexeme("en.test.active", "zzzactiveattack", "core"),
            fixture_lexeme("en.test.disabled", "zzzdisabledattack", "disabled")
        ),
    );

    let reg = build_registry_at(root).expect("build with a disabled lexeme");
    assert!(
        reg.by_id("en.test.disabled").is_none(),
        "a disabled lexeme must be absent from the registry"
    );
    assert!(
        reg.lexemes().iter().all(|l| l.id != "en.test.disabled"),
        "a disabled lexeme must not appear in `lexemes()`"
    );

    let matcher = LexiconMatcher::from_lexemes(reg.lexemes());
    assert!(
        !matcher
            .find_iter("they zzzdisabledattack now")
            .any(|m| m.id == "en.test.disabled"),
        "the matcher must not report a disabled lexeme"
    );
    assert!(
        matcher
            .find_iter("they zzzactiveattack now")
            .any(|m| m.id == "en.test.active"),
        "the matcher must still report an active lexeme"
    );
}

/// Objective: Verify the failure path `try_init` surfaces — a malformed
/// core lexicon is reported as an error rather than silently becoming an
/// empty registry (audit H18).
/// Invariants: `build_registry_at` returns `Err` for invalid core JSON.
#[test]
fn broken_core_lexicon_is_reported() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    write_fixture(
        &dir.path().join("config/dictionary.json"),
        r#"{"lexemes": [ this is not json"#,
    );
    let result = build_registry_at(dir.path());
    assert!(
        result.is_err(),
        "a malformed core lexicon must surface as an Err, got {result:?}"
    );
}

/// Objective: Verify one broken optional pack is skipped (and logged) rather
/// than taking the whole lexicon down, so core keeps working.
/// Invariants: the build succeeds and the core lexeme is still present.
#[test]
fn broken_pack_is_skipped_without_failing_the_build() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path();
    write_fixture(
        &root.join("config/dictionary.json"),
        &format!(
            r#"{{"lexemes":[{}]}}"#,
            fixture_lexeme("en.test.coreonly", "coreonlyword", "core")
        ),
    );
    write_fixture(&root.join("lexicon/packs/broken.json"), "{ not json");

    let reg = build_registry_at(root).expect("a broken optional pack must not fail the build");
    assert!(
        reg.by_id("en.test.coreonly").is_some(),
        "core must survive a broken optional pack"
    );
}

/// Objective: Verify the global singleton reports a clean load when the
/// shipped config is present, so `try_init` is `Ok` in the normal case.
/// Invariants: `try_init()` returns `Ok(())`.
#[test]
fn try_init_succeeds_with_the_shipped_lexicon() {
    assert!(try_init().is_ok(), "the shipped lexicon must build cleanly");
}
