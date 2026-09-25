//! Unit tests for the conversation compiler (marker channel + fact conversion).
//!
//! They live in a sibling file instead of the end of `mod.rs` so that neither
//! file crosses the one-file-per-1000-lines rule (`plan/rules/rules.md` §1).

use super::markers::merge_marker_files;
use super::*;

/// Objective: Verify the two shipped marker files merge — Chinese words
/// (幸福 → feel) and English words (excited → feel, adore → love) both
/// contribute; users can customize each language's file independently.
/// Invariants: merged pairs contain zh and en markers with their actions.
#[test]
fn marker_files_merge_zh_and_en() {
    let dir = tempfile::tempdir().expect("tempdir");
    let zh = dir.path().join("markers_zh.json");
    let en = dir.path().join("markers_en.json");
    std::fs::write(&zh, r#"{"feel": ["幸福", "失眠"], "plan": ["目标"]}"#).expect("write zh");
    std::fs::write(
        &en,
        r#"{"feel": ["excited", "burned out"], "love": ["adore"]}"#,
    )
    .expect("write en");

    let pairs = merge_marker_files(&[zh, en]);
    assert!(
        pairs.contains(&("幸福".to_string(), "feel".to_string())),
        "zh feel marker merged, got {pairs:?}"
    );
    assert!(
        pairs.contains(&("目标".to_string(), "plan".to_string())),
        "zh plan marker merged, got {pairs:?}"
    );
    assert!(
        pairs.contains(&("excited".to_string(), "feel".to_string())),
        "en feel marker merged, got {pairs:?}"
    );
    assert!(
        pairs.contains(&("adore".to_string(), "love".to_string())),
        "en love marker merged, got {pairs:?}"
    );
}

/// Objective: Verify the fail-soft fallback — when BOTH marker files are
/// missing/corrupt, the built-in default table is used so a config-less
/// deployment keeps compiling facts.
/// Invariants: merge of two nonexistent paths yields the default table
/// (non-empty, includes the core 喜欢 and feel markers).
#[test]
fn marker_files_fallback_to_defaults_when_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("does-not-exist.json");
    let pairs = merge_marker_files(&[missing.clone(), missing]);
    assert!(
        !pairs.is_empty(),
        "fallback must be non-empty, got {pairs:?}"
    );
    assert!(
        pairs.contains(&("喜欢".to_string(), "喜欢".to_string())),
        "default 喜欢 marker present, got {pairs:?}"
    );
    assert!(
        pairs.contains(&("疲惫".to_string(), "feel".to_string())),
        "default feel marker present, got {pairs:?}"
    );
    assert!(
        pairs.contains(&("love".to_string(), "love".to_string())),
        "default English marker present, got {pairs:?}"
    );
}

/// Objective: Verify one valid + one missing file still yields the valid
/// file's markers (partial load is not an all-or-nothing failure).
/// Invariants: only the valid zh file's markers appear.
#[test]
fn marker_files_partial_load_keeps_valid_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let zh = dir.path().join("markers_zh.json");
    std::fs::write(&zh, r#"{"want": ["想学"]}"#).expect("write zh");
    let missing = dir.path().join("markers_en.json");

    let pairs = merge_marker_files(&[zh, missing]);
    assert!(
        pairs.contains(&("想学".to_string(), "want".to_string())),
        "valid file markers kept, got {pairs:?}"
    );
    assert_eq!(pairs.len(), 1, "no extra markers from the missing file");
}

#[test]
fn compile_extracts_knowledge() {
    let compiler = ConversationCompiler::new();
    let msgs = vec![
        Message::new("user", "为什么编译那么慢？"),
        Message::new("assistant", "因为lancedb太重"),
    ];
    let r = compiler.compile("t1", &msgs);
    assert!(!r.knowledge.is_empty());
}

#[test]
fn compile_tracks_files() {
    let compiler = ConversationCompiler::new();
    let msgs = vec![Message::new("user", "修改compiler.rs 和 prompt.rs")];
    let r = compiler.compile("t1", &msgs);
    assert!(r.session.current_files.contains(&"compiler.rs".to_string()));
    assert!(r.session.current_files.contains(&"prompt.rs".to_string()));
}

#[test]
fn compile_first_user_msg_is_goal() {
    let compiler = ConversationCompiler::new();
    let msgs = vec![
        Message::new("user", "帮我实现prompt模块"),
        Message::new("assistant", "好的"),
    ];
    let r = compiler.compile("t1", &msgs);
    assert!(r.session.current_goal.contains("prompt"));
}

#[test]
fn compile_unanswered_is_open_problem() {
    let compiler = ConversationCompiler::new();
    let msgs = vec![
        Message::new("user", "有个bug"),
        Message::new("assistant", "修好了"),
        Message::new("user", "性能太差"),
    ];
    let r = compiler.compile("t1", &msgs);
    assert!(r.session.open_problems.iter().any(|p| p.contains("性能")));
}

/// Objective: Verify ordinary emotion/preference messages now produce
/// observations (the expanded marker set). Previously only 12 hardcoded
/// words matched, so "我很焦虑，压力很大" compiled zero facts and the
/// facts table stayed empty.
/// Invariants: each emotion message yields an observation carrying the
/// original content as evidence.
#[test]
fn compile_user_observations_covers_emotion_and_preference() {
    let msgs = vec![
        Message::new("user", "我很焦虑，压力很大。"),
        Message::new("user", "我欣赏曹操的知人善任。"),
        Message::new("user", "我打算下周发布新版本。"),
    ];
    let obs = compile_user_observations(&msgs, 1);
    assert!(
        obs.iter()
            .any(|o| o.action == "feel" && o.evidence.is_some()),
        "焦虑/压力 must yield a feel observation, got {obs:?}"
    );
    assert!(
        obs.iter()
            .any(|o| o.action == "喜欢" && o.evidence.is_some()),
        "欣赏 must yield a 喜欢 observation, got {obs:?}"
    );
    assert!(
        obs.iter().any(|o| o.action == "plan"),
        "打算 must yield a plan observation, got {obs:?}"
    );
}

/// Objective: Verify MODERN vernacular speech — words the classic novel
/// lexicon does not carry (加班/应酬/失眠/疲惫/孤独) — now yields feel
/// observations. Daily-life conversations previously compiled zero facts.
/// Invariants: a message about overwork/insomnia produces a feel
/// observation; a loneliness message produces one too.
#[test]
fn modern_vernacular_speech_produces_observations() {
    let msgs = vec![
        Message::new("user", "最近天天加班，应酬也多，晚上还失眠。"),
        Message::new("user", "一个人在外地，有时候挺孤独的。"),
    ];
    let obs = compile_user_observations(&msgs, 1);
    assert!(
        obs.iter()
            .any(|o| o.action == "feel" && o.evidence.is_some()),
        "加班/应酬/失眠 must yield a feel observation, got {obs:?}"
    );
    let evidence_texts: Vec<&str> = obs
        .iter()
        .filter_map(|o| o.evidence.as_ref().map(|e| e.text.as_str()))
        .collect();
    assert!(
        evidence_texts.iter().any(|t| t.contains("加班")),
        "evidence must reference the 加班 message, got {evidence_texts:?}"
    );
    assert!(
        evidence_texts.iter().any(|t| t.contains("孤独")),
        "孤独 message must also produce evidence, got {evidence_texts:?}"
    );
}

/// Objective: Verify the broadened high-frequency Chinese vocabulary —
/// positive emotions (幸福/高兴/满足), negative emotions (烦恼/绝望/
/// 崩溃), plans (目标/梦想), and wants (想学/想去) — all produce
/// observations with evidence, and that multi-char markers prevent
/// single-char false positives ("想" alone would match 想象/想法).
/// Invariants: each category message yields its mapped action.
#[test]
fn high_frequency_chinese_vocabulary_produces_observations() {
    let msgs = vec![
        Message::new("user", "今天很幸福，也很高兴能和你聊天。"),
        Message::new("user", "最近特别烦恼，感觉快崩溃了。"),
        Message::new("user", "我的目标是明年发布自己的产品，这是我的梦想。"),
        Message::new("user", "我想学钢琴，想去欧洲旅行。"),
    ];
    let obs = compile_user_observations(&msgs, 1);
    let evidence_texts: Vec<&str> = obs
        .iter()
        .filter_map(|o| o.evidence.as_ref().map(|e| e.text.as_str()))
        .collect();
    assert!(
        evidence_texts.iter().any(|t| t.contains("幸福")),
        "幸福 must yield evidence, got {evidence_texts:?}"
    );
    assert!(
        evidence_texts.iter().any(|t| t.contains("崩溃")),
        "崩溃 must yield evidence, got {evidence_texts:?}"
    );
    assert!(
        obs.iter()
            .any(|o| o.action == "plan" && o.evidence.is_some()),
        "目标/梦想 must yield a plan observation, got {obs:?}"
    );
    assert!(
        obs.iter()
            .any(|o| o.action == "want" && o.evidence.is_some()),
        "想学/想去 must yield a want observation, got {obs:?}"
    );
    // Single-char 想 must not fire on 想象: "想" alone is not a marker.
    let imagination = Message::new("user", "我的想象里有一片海。");
    let obs2 = compile_user_observations(std::slice::from_ref(&imagination), 1);
    assert!(
        obs2.is_empty(),
        "想象 must NOT match a bare 想 marker, got {obs2:?}"
    );
}

/// Objective: Verify duplicate-fact inflation is fixed — one message
/// matching several SAME-action markers must yield a single observation
/// for that action (失眠+加班+压力+好累 all → feel, so ONE feel obs, not
/// four near-identical ones), while DIFFERENT actions are kept.
/// Invariants: exactly one feel observation for the four-marker sentence;
/// a mixed sentence yields one obs per distinct action.
#[test]
fn same_action_markers_dedup_to_single_observation() {
    // All four markers map to `feel`: previously 4 observations.
    let msgs = vec![Message::new("user", "最近天天失眠，还加班，压力好大，好累")];
    let obs = compile_user_observations(&msgs, 1);
    let feel_count = obs.iter().filter(|o| o.action == "feel").count();
    assert_eq!(
        feel_count, 1,
        "four feel markers in one sentence must yield ONE feel observation, got {obs:?}"
    );
    // Different actions in one sentence are all kept.
    let mixed = vec![Message::new("user", "我打算学钢琴，也很开心")];
    let obs2 = compile_user_observations(&mixed, 1);
    let actions: Vec<&str> = obs2.iter().map(|o| o.action.as_str()).collect();
    assert!(
        actions.contains(&"plan"),
        "plan marker kept alongside feel, got {actions:?}"
    );
    assert!(
        actions.contains(&"feel"),
        "feel marker kept alongside plan, got {actions:?}"
    );
}

#[test]
fn compile_detects_decision_via_done() {
    let compiler = ConversationCompiler::new();
    let msgs = vec![
        Message::new("user", "把lancedb换成sqlite-vec行不行？"),
        Message::new("assistant", "Done，已经替换了"),
    ];
    let r = compiler.compile("t1", &msgs);
    assert!(!r.decisions.is_empty());
    assert!(r.decisions[0].decision.contains("lancedb"));
}

#[test]
fn compile_knowledge_deduped() {
    let compiler = ConversationCompiler::new();
    let msgs = vec![
        Message::new("user", "为什么慢？"),
        Message::new("assistant", "因为lancedb太重"),
        Message::new("user", "为什么慢？"),
        Message::new("assistant", "因为lancedb太重"),
    ];
    let result = compiler.compile("t1", &msgs);
    assert!(
        result.knowledge.len() <= 1,
        "Equivalent knowledge should be emitted at most once"
    );
}

/// Objective: Verify conversation input crosses the unified Observation IR.
/// Invariants: Assistant text is ignored and user goals retain source evidence.
#[test]
fn user_observations_only_compile_user_authored_cognition() {
    let messages = vec![
        Message::new("user", "我准备找一份 Rust 工作"),
        Message::new("assistant", "我喜欢 Go"),
    ];

    let observations = compile_user_observations(&messages, 42);

    assert_eq!(
        observations.len(),
        1,
        "Only the user's goal should become an observation"
    );
    assert_eq!(
        observations[0].subject.entity_id,
        Some(42),
        "Observation should target the configured User entity"
    );
    assert_eq!(
        observations[0].action, "准备",
        "The goal marker should retain its semantic action"
    );
    assert!(
        observations[0].evidence.is_some(),
        "Every conversation observation should retain source evidence"
    );
}

/// Objective: Verify user observations become typed immutable facts.
/// Invariants: Goal and emotion facts share the User id and supplied logical time.
#[test]
fn user_facts_preserve_type_entity_and_time() {
    let messages = vec![Message::new("user", "我计划学 Rust，但最近压力很大")];

    let facts = compile_user_facts(&messages, 99, 20260730);

    assert_eq!(
        facts.len(),
        2,
        "Both goal and emotion signals should produce facts"
    );
    assert!(
        facts.iter().any(|fact| fact.fact_type == FactType::Goal),
        "Plan signal should compile to a Goal fact"
    );
    assert!(
        facts.iter().any(|fact| fact.fact_type == FactType::Emotion),
        "Stress signal should compile to an Emotion fact"
    );
    assert!(
        facts.iter().all(|fact| fact.entity_id == 99),
        "All facts should belong to the configured User entity"
    );
    assert!(
        facts.iter().all(|fact| fact.time == 20260730),
        "All facts should retain the supplied logical time"
    );
}

/// Objective: Verify a negated preference is recorded as a negated state
/// instead of being dropped. Discarding it made a user's negative stance
/// unrepresentable and left `StanceFlip` unreachable for a user entity
/// (negated facts existed only for the agent's own persona).
/// Invariants: "我不喜欢应酬" keeps its `喜欢` (Preference) fact and its
/// `应酬` (Emotion) fact, each tagged `negated: true` with the verbatim
/// message preserved as content.
#[test]
fn negated_preference_is_recorded_as_a_negated_state() {
    let messages = vec![Message::new("user", "我不喜欢应酬")];

    let facts = compile_user_facts(&messages, 99, 20260730);

    let preference = facts
        .iter()
        .find(|fact| fact.fact_type == FactType::Preference)
        .expect("the 喜欢 marker must still compile to a Preference fact");
    assert_eq!(
        preference.payload["negated"],
        serde_json::Value::Bool(true),
        "the negation must be recorded on the fact"
    );
    assert_eq!(
        preference.payload["content"], "我不喜欢应酬",
        "the verbatim message is preserved"
    );
    assert!(
        facts
            .iter()
            .all(|fact| fact.payload["negated"] == serde_json::Value::Bool(true)),
        "every fact from a negated message carries the negation, got {facts:?}"
    );
}

/// Objective: Verify a negated plan never surfaces as an AFFIRMATIVE goal
/// (ELITE_LEXICON_PLAN §13.3) while still being remembered as the negated
/// state it is. Discarding the fact kept the rule but lost the information:
/// "我不打算考公务员了" is exactly the kind of change a companion must not miss.
/// Invariants: a Goal fact is produced, and it carries `negated: true`;
/// exactly one Goal fact exists (no mirrored affirmative twin).
#[test]
fn negated_plan_is_kept_as_a_negated_goal() {
    let messages = vec![Message::new("user", "我不打算学 Rust")];

    let facts = compile_user_facts(&messages, 99, 20260730);
    let goals: Vec<&Fact> = facts
        .iter()
        .filter(|fact| fact.fact_type == FactType::Goal)
        .collect();

    assert_eq!(
        goals.len(),
        1,
        "the plan must be remembered once: {facts:?}"
    );
    assert_eq!(
        goals[0].payload["negated"],
        serde_json::Value::Bool(true),
        "the goal must be stored as negated, so the current-state projection \
             never lists it as an active plan"
    );
    assert!(
        !facts.iter().any(|fact| fact.fact_type == FactType::Goal
            && fact.payload["negated"] != serde_json::Value::Bool(true)),
        "no affirmative goal may be produced from a negated plan, got {facts:?}"
    );
}

/// Objective: Verify negation is resolved per marker in its own clause, not
/// per message. A message-wide flag turned any sentence containing 不/没/别
/// into a negated one: "想学吉他很久了，一直在纠结买不买" lost its plan
/// entirely, because a cue in a later clause ("买不买") marked the whole
/// message negated and negated plans were dropped.
/// Invariants: the plan survives and is affirmative; the `买不买` cue does not
/// touch it.
#[test]
fn negation_is_resolved_per_clause() {
    let messages = vec![Message::new("user", "想学吉他很久了，一直在纠结买不买")];

    let facts = compile_user_facts(&messages, 99, 20260730);
    let goal = facts
        .iter()
        .find(|fact| fact.fact_type == FactType::Goal)
        .unwrap_or_else(|| panic!("the plan must survive a cue in another clause: {facts:?}"));

    assert_eq!(
        goal.payload["negated"],
        serde_json::Value::Bool(false),
        "a cue in a different clause must not negate this plan: {facts:?}"
    );
}
