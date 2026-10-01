//! Unit tests for profile extraction and entity discovery.
//!
//! They live in a sibling file instead of the end of `profile.rs` so neither
//! file crosses the one-file-per-1000-lines rule (`plan/rules/rules.md` §1) —
//! the same split `conversation_compiler/tests.rs` uses. They keep private
//! access to the module (a child module sees its parent's private items), which
//! an integration test under `tests/` could not.

use super::*;

fn make_dict() -> EntityDictionary {
    let mut d = EntityDictionary::default();
    d.alias_to_canonical.insert("刘备".into(), "刘备".into());
    d.alias_to_canonical.insert("玄德".into(), "刘备".into());
    d.alias_to_canonical.insert("关羽".into(), "关羽".into());
    d.alias_to_canonical.insert("云长".into(), "关羽".into());
    d.alias_to_canonical.insert("张飞".into(), "张飞".into());
    d
}

/// Objective: Verify the person-name validator accepts real names and
/// rejects the garbage the heuristic produced on 大秦帝国.
/// Invariants: common names + compound surnames pass; function-word/noun
/// tails / non-surname heads / bad shapes are rejected.
#[test]
fn person_name_validation_gates() {
    // C1: shape.
    assert!(!is_valid_person_name("嬴"), "single char is not a name");
    assert!(!is_valid_person_name("嬴渠梁一"), "5 chars rejected");
    assert!(!is_valid_person_name("abc"), "non-CJK rejected");

    // C2: surname-led (incl. compound).
    assert!(is_valid_person_name("嬴渠梁"), "嬴 is a surname");
    assert!(is_valid_person_name("商鞅"), "商 is a surname");
    assert!(is_valid_person_name("吕不韦"), "吕 is a surname");
    assert!(is_valid_person_name("公孙鞅"), "compound surname 公孙");
    assert!(is_valid_person_name("司马错"), "compound surname 司马");
    assert!(!is_valid_person_name("涓的秘"), "涓 is not a surname");

    // C3: function words anywhere.
    assert!(!is_valid_person_name("涓的秘密"), "ends with 的");
    assert!(!is_valid_person_name("的感觉却"), "ends with 却");
    assert!(
        !is_valid_person_name("一金令"),
        "starts with function word 一"
    );

    // C4: noun tails.
    assert!(!is_valid_person_name("牛角"), "ends with noun 角");
    assert!(!is_valid_person_name("白绢衣裤"), "ends with noun 裤");
    assert!(!is_valid_person_name("一金令箭"), "ends with noun 箭");
    assert!(!is_valid_person_name("文明时代"), "ends with noun 代");

    // Sanity: real person with a noun-looking char in the MIDDLE still
    // passes (only the TAIL is gated by C4). `王金城` would be rejected
    // because 城 is a noun tail — that is the intended C4 behavior.
    assert!(
        is_valid_person_name("李金诚"),
        "name containing 金 mid-name is fine"
    );
}

/// Objective: Verify the heuristic garbage from the 大秦帝国 baseline is
/// now filtered by the constraint inside `extract_profiles`.
/// Invariants: a line that used to yield `涓的秘密`-style entities now
/// yields none, while real introduction lines still extract.
#[test]
fn validation_filters_baseline_garbage() {
    let lang = crate::language::ChineseLanguageProvider::new();
    let mut ctx = CompileContext {
        document_title: "test".into(),
        ..Default::default()
    };
    // Garbage-prone line: marker 也 preceded by a non-name fragment.
    profile_lines_garbage(&mut ctx, &lang);
    assert!(
        ctx.entities.iter().all(|e| is_valid_person_name(&e.name)),
        "all extracted entities must pass the name validator"
    );
}

/// Feed lines that previously produced garbage entities and assert no
/// invalid entity survives.
fn profile_lines_garbage(
    ctx: &mut CompileContext,
    lang: &crate::language::ChineseLanguageProvider,
) {
    // These are the kind of fragments the baseline mis-discovered.
    for line in [
        "涓的秘密者也。",
        "一金令箭者也。",
        "白绢衣裤者也。",
        "牛角者也。",
    ] {
        extract_profiles(line, ctx, None, &[], lang);
    }
    assert!(
        ctx.entities.is_empty(),
        "garbage fragments must yield no entities, got {:?}",
        ctx.entities
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    );
}

/// Objective: Verify that "字玄德" after "刘备" extracts courtesy_name.
/// Invariants: Profile key "courtesy_name" with value "玄德".
#[test]
fn courtesy_from_dialog() {
    let mut ctx = CompileContext::default();
    extract_profiles(
        "刘备字玄德，涿郡人也",
        &mut ctx,
        Some(&make_dict()),
        &[],
        &crate::language::ChineseLanguageProvider::new(),
    );
    let cp = ctx.profiles.iter().find(|p| p.key == "courtesy_name");
    assert!(cp.is_some(), "courtesy_name should be extracted");
    assert_eq!(
        cp.unwrap().value,
        "玄德",
        "字 must capture the courtesy name"
    );
    assert!(
        ctx.entities.iter().any(|e| e.name == "刘备"),
        "刘备 entity created"
    );
}

/// Objective: Verify that birthplace is extracted from "XX人也".
/// Invariants: Profile key "birthplace" with the region name.
#[test]
fn birthplace_extracted() {
    let mut ctx = CompileContext::default();
    extract_profiles(
        "张飞涿郡人也",
        &mut ctx,
        Some(&make_dict()),
        &[],
        &crate::language::ChineseLanguageProvider::new(),
    );
    let bp = ctx.profiles.iter().find(|p| p.key == "birthplace");
    assert!(bp.is_some(), "birthplace should be extracted");
    assert!(
        bp.unwrap().value.contains("涿郡"),
        "the birth place must be captured"
    );
}

/// Objective: Verify that weapon is extracted from "使XX".
/// Invariants: Profile key "weapon" with the weapon name.
#[test]
fn weapon_extracted() {
    let mut ctx = CompileContext::default();
    extract_profiles(
        "关羽使青龙偃月刀",
        &mut ctx,
        Some(&make_dict()),
        &[],
        &crate::language::ChineseLanguageProvider::new(),
    );
    let wp = ctx.profiles.iter().find(|p| p.key == "weapon");
    assert!(wp.is_some(), "weapon should be extracted");
    assert_eq!(
        wp.unwrap().value,
        "青龙偃月刀",
        "the weapon slot must capture the full name"
    );
}

/// Objective: Verify that narrative text without entity names produces nothing.
/// Invariants: No entities or profiles created.
#[test]
fn narrative_text_ignored() {
    let mut ctx = CompileContext::default();
    extract_profiles(
        "话说天下大势，分久必合",
        &mut ctx,
        Some(&make_dict()),
        &[],
        &crate::language::ChineseLanguageProvider::new(),
    );
    assert!(ctx.entities.is_empty(), "no entity for narrative text");
    assert!(ctx.profiles.is_empty(), "no profiles for narrative text");
}

/// Objective: Verify that alias mention ("玄德") resolves to canonical name ("刘备").
/// Invariants: Entity created with name "刘备", not "玄德".
#[test]
fn alias_resolves_to_canonical() {
    let mut ctx = CompileContext::default();
    extract_profiles(
        "玄德幼孤，事母至孝",
        &mut ctx,
        Some(&make_dict()),
        &[],
        &crate::language::ChineseLanguageProvider::new(),
    );
    for entity in &ctx.entities {
        assert_ne!(
            entity.name, "玄德",
            "Entity names should be canonical rather than aliases"
        );
    }
}

/// Objective: Verify that `。` acts as a sentence boundary and is never
/// skipped when walking back to a name (NEW-H24 regression lock).
/// Invariants: "话说。刘备字玄德" must discover "刘备" — never the garbage
/// "说话刘备" that would result from crossing the `。` boundary.
#[test]
fn sentence_boundary_stops_name_walk() {
    let mut ctx = CompileContext::default();
    extract_profiles(
        "话说。刘备字玄德，涿郡人也。",
        &mut ctx,
        Some(&make_dict()),
        &[],
        &crate::language::ChineseLanguageProvider::new(),
    );
    let names: Vec<&str> = ctx.entities.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"刘备"),
        "`。`-boundary walk must still find 刘备, got {names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|n| n.contains("说话") || n.contains("话说")),
        "name walk must not cross the `。` sentence boundary, got {names:?}"
    );
}

/// Objective: Verify `find_entity_in_text` is deterministic for
/// same-length aliases (NEW-H25) and excludes single-char aliases (the
/// `AliasIndex::build` rule — no context safety on a whole-text scan,
/// so "云" must never resolve inside "浮云"). Invariants: one stable
/// entity; 浮云蔽日 → None with a single-char alias present.
#[test]
fn same_length_alias_resolution_is_deterministic() {
    let dict = make_dict();
    let mut results = std::collections::HashSet::new();
    for _ in 0..50 {
        let hit = find_entity_in_text("关羽字云长", &dict);
        results.insert(hit.map(|(name, _)| name));
    }
    assert_eq!(
        results.len(),
        1,
        "same-length alias resolution must be stable across runs, got {results:?}"
    );

    // Single-char exclusion (AliasIndex::build rule).
    let mut dict = make_dict();
    dict.alias_to_canonical.insert("云".into(), "赵云".into());
    dict.alias_to_canonical.insert("赵云".into(), "赵云".into());
    assert!(
        find_entity_in_text("浮云蔽日", &dict).is_none(),
        "single-char 云 must not resolve inside 浮云"
    );
    assert_eq!(
        find_entity_in_text("赵云观阵", &dict).map(|(n, _)| n),
        Some("赵云".to_string()),
        "multi-char alias still resolves"
    );
}

/// Objective: Verify the English frontend discovers titled personal names.
/// Invariants: The entity retains title plus name and a title profile.
#[test]
fn english_title_discovers_person_entity() {
    let mut context = CompileContext::default();
    extract_profiles(
        "Prince Andrei Bolkonsky was the son of Prince Nikolai Bolkonsky.",
        &mut context,
        None,
        &[],
        &crate::language::EnglishLanguageProvider::new(),
    );

    assert!(
        context
            .entities
            .iter()
            .any(|entity| entity.name == "Prince Andrei Bolkonsky"),
        "English title discovery should preserve the titled personal name"
    );
    assert!(
        context
            .profiles
            .iter()
            .any(|profile| profile.key == "title" && profile.value == "Prince"),
        "English title discovery should emit a title profile"
    );
}

/// Objective: Verify dotted English honorifics survive normalization.
/// Invariants: Each discovered entity preserves its canonical dotted title and profile.
#[test]
fn dotted_english_titles_are_preserved() {
    let mut context = CompileContext::default();
    extract_profiles(
        "Mr. Bennet greeted Mrs. Bennet, while Mr. Darcy waited.",
        &mut context,
        None,
        &[],
        &crate::language::EnglishLanguageProvider::new(),
    );

    assert!(
        context
            .entities
            .iter()
            .any(|entity| entity.name == "Mr. Bennet"),
        "Dotted honorific normalization must preserve `Mr. Bennet`; found {:?}",
        context
            .entities
            .iter()
            .map(|entity| entity.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        context
            .profiles
            .iter()
            .any(|profile| profile.key == "title" && profile.value == "Mr."),
        "A discovered dotted honorific must create the matching title profile"
    );
}

/// Objective: Verify Project Gutenberg administrative text is not literary evidence.
/// Invariants: License headings produce neither entities nor title profiles.
#[test]
fn gutenberg_metadata_is_not_a_person() {
    let mut context = CompileContext::default();
    extract_profiles(
        "General Terms of Use and Redistributing Project Gutenberg-tm electronic works\nGeneral Information About Project Gutenberg-tm electronic works",
        &mut context,
        None,
        &[],
        &crate::language::EnglishLanguageProvider::new(),
    );

    assert!(
        context.entities.is_empty(),
        "Gutenberg administrative headings must not create people; found {:?}",
        context
            .entities
            .iter()
            .map(|entity| entity.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        context.profiles.is_empty(),
        "Gutenberg administrative headings must not create title profiles"
    );
}
