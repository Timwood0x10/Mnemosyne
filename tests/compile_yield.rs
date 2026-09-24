//! Compile-yield measurement: does the engine actually remember what a
//! companion user says?
//!
//! Every previous round of review found the same root cause — behaviour that was
//! only ever exercised with hand-crafted payloads — so this harness drives the
//! **real** compiler over an annotated corpus of colloquial companion dialogue
//! (`tests/fixtures/compile_yield_zh.json`) and reports:
//!
//! - `must_catch` recall: explicit signals the marker table is designed for.
//!   This is a regression floor, not an aspiration.
//! - `should_catch` recall: what a human would expect the engine to remember but
//!   the current mechanism may not reach (identity/relationship/interest,
//!   indirect statements, capitalised English). Measured, not asserted.
//! - phantom rate: filler and small talk must produce **zero** facts.
//! - over-extraction: facts on an annotated line whose type the annotation does
//!   not accept.
//! - commitment extraction (`src/commitment.rs`) is measured separately, because
//!   promises do not come from the marker table.
//!
//! Per-fact invariants are asserted on every line: each fact must carry its
//! source sentence as `payload.content` and as a traceable `payload.evidence`
//! anchor, and compiling twice must be byte-identical.

use std::collections::BTreeSet;

use mnemosyne::commitment::commitments_from_messages;
use mnemosyne::conversation_compiler::compile_user_facts;
use mnemosyne::types::Message;
use serde_json::Value;

/// Corpus path, resolved at compile time so the test never depends on the CWD.
const CORPUS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/compile_yield_zh.json"
);

/// Regression floors, set from the measured baseline (see the report emitted by
/// this test). They exist to catch silent quality regressions when marker tables
/// or extraction rules change — raise them as the engine improves.
///
/// Baseline 2026-09-23: `must_catch` 30/30 (100%), `should_catch` 2/9 (22%),
/// phantoms 0/9. The `must_catch` floor is deliberately at 100%: an explicit
/// signal ("我特别喜欢吃…") must never silently stop producing a fact.
const MUST_CATCH_RECALL_PCT: usize = 100;
const SHOULD_CATCH_RECALL_PCT: usize = 22;

/// One annotation tier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    /// The marker table is designed for this; a miss is a regression.
    MustCatch,
    /// Desirable; a miss is a measured capability gap.
    ShouldCatch,
    /// Filler; any fact here is a phantom.
    MustIgnore,
}

impl Tier {
    fn parse(value: &str) -> Self {
        match value {
            "must_catch" => Tier::MustCatch,
            "should_catch" => Tier::ShouldCatch,
            "must_ignore" => Tier::MustIgnore,
            other => panic!("unknown tier `{other}` in the corpus"),
        }
    }
}

/// One annotated utterance.
struct Line {
    text: String,
    tier: Tier,
    /// Accepted `FactType` names (`FactType::as_str`), empty when none allowed.
    expect: Vec<String>,
}

/// Route the report through `tracing`, as the testing rules require.
fn init_reporting() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();
}

/// Read the annotated corpus.
fn corpus() -> Value {
    let raw = std::fs::read_to_string(CORPUS)
        .unwrap_or_else(|error| panic!("the corpus at {CORPUS} must be readable: {error}"));
    serde_json::from_str(&raw).expect("the corpus must be valid JSON")
}

/// Flatten every session line into annotation order.
fn lines(document: &Value) -> Vec<Line> {
    let mut lines = Vec::new();
    for session in document["sessions"]
        .as_array()
        .expect("the corpus carries sessions")
    {
        for line in session["lines"]
            .as_array()
            .expect("every session carries lines")
        {
            lines.push(Line {
                text: line["text"]
                    .as_str()
                    .expect("every line has text")
                    .to_string(),
                tier: Tier::parse(line["tier"].as_str().expect("every line has a tier")),
                expect: line["expect"]
                    .as_array()
                    .expect("every line has an expectation")
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .expect("expectations are strings")
                            .to_string()
                    })
                    .collect(),
            });
        }
    }
    lines
}

/// Compile one line and assert the per-fact invariants.
fn compile_line(line: &str, time: i32) -> Vec<mnemosyne::cognition::Fact> {
    let facts = compile_user_facts(&[Message::new("user", line)], 7, time);
    for fact in &facts {
        assert_eq!(
            fact.payload["content"].as_str(),
            Some(line),
            "every fact must carry the sentence it came from, got {fact:?}"
        );
        assert_eq!(
            fact.payload["evidence"]["text"].as_str(),
            Some(line),
            "every fact must carry a traceable evidence anchor, got {fact:?}"
        );
    }
    facts
}

/// Objective: Measure the compiler's yield over the annotated corpus and lock the
/// current quality in place. The engine's promise is "a companion that remembers
/// you", and that promise had never been quantified: the fixtures were a few
/// hundred bytes and no test measured whether real colloquial speech produces the
/// right facts.
/// Invariants: filler produces ZERO facts; every fact on an annotated line is
/// either an accepted type or counted as over-extraction; every fact carries its
/// source sentence and evidence anchor; compilation is deterministic;
/// `must_catch` recall stays at or above the regression floor; commitment
/// extraction returns exactly the annotated number of decisions with the
/// annotated verb.
#[test]
fn compile_yield_matches_the_annotated_corpus() {
    init_reporting();
    let document = corpus();
    let annotated = lines(&document);

    let mut must_total = 0usize;
    let mut must_hit = 0usize;
    let mut should_total = 0usize;
    let mut should_hit = 0usize;
    let mut ignore_total = 0usize;
    let mut phantoms: Vec<String> = Vec::new();
    let mut over_extracted: Vec<String> = Vec::new();
    let mut must_misses: Vec<String> = Vec::new();
    let mut should_misses: Vec<String> = Vec::new();
    let mut negated_checked = 0usize;

    for (index, line) in annotated.iter().enumerate() {
        // One logical time per line keeps the corpus deterministic.
        let time = 2026 + index as i32;
        let facts = compile_line(&line.text, time);
        let produced: BTreeSet<&str> = facts.iter().map(|fact| fact.fact_type.as_str()).collect();

        // Determinism: the same input must produce the same facts.
        let second = compile_user_facts(&[Message::new("user", &line.text)], 7, time);
        assert_eq!(
            serde_json::to_value(&facts).expect("facts serialize"),
            serde_json::to_value(&second).expect("facts serialize"),
            "compiling the same utterance twice must be identical: `{}`",
            line.text
        );

        // Over-extraction is measured on every line that expects something.
        if !line.expect.is_empty() {
            for fact in &facts {
                let kind = fact.fact_type.as_str();
                if !line.expect.iter().any(|expected| expected == kind) {
                    over_extracted.push(format!("`{}` → {kind}", line.text));
                }
            }
        }

        // A negated statement must never be stored as an affirmative fact.
        if line.text.contains("我不喜欢应酬") {
            negated_checked += 1;
            assert!(
                !facts.is_empty(),
                "the negation corpus line must still produce facts"
            );
            for fact in &facts {
                assert_eq!(
                    fact.payload["negated"], true,
                    "`{}` must be stored as a negated fact, got {fact:?}",
                    line.text
                );
            }
        }

        let hit = line
            .expect
            .iter()
            .any(|expected| produced.contains(expected.as_str()));
        match line.tier {
            Tier::MustCatch => {
                must_total += 1;
                if hit {
                    must_hit += 1;
                } else {
                    must_misses.push(format!(
                        "`{}` (expected {:?}, produced {:?})",
                        line.text,
                        line.expect,
                        facts
                            .iter()
                            .map(|fact| fact.fact_type.as_str())
                            .collect::<Vec<_>>()
                    ));
                }
            }
            Tier::ShouldCatch => {
                should_total += 1;
                if hit {
                    should_hit += 1;
                } else {
                    should_misses.push(format!(
                        "`{}` (expected {:?}, produced {:?})",
                        line.text,
                        line.expect,
                        facts
                            .iter()
                            .map(|fact| fact.fact_type.as_str())
                            .collect::<Vec<_>>()
                    ));
                }
            }
            Tier::MustIgnore => {
                ignore_total += 1;
                if !facts.is_empty() {
                    phantoms.push(format!(
                        "`{}` → {:?}",
                        line.text,
                        facts
                            .iter()
                            .map(|fact| fact.fact_type.as_str())
                            .collect::<Vec<_>>()
                    ));
                }
            }
        }
    }

    // Commitments are a separate extraction path.
    let mut commitment_failures: Vec<String> = Vec::new();
    let cases = document["commitments"]["cases"]
        .as_array()
        .expect("the corpus carries commitment cases");
    for case in cases {
        let text = case["text"].as_str().expect("a case has text");
        let expected_count = case["decisions"].as_u64().expect("a case has a count") as usize;
        let expected_verb = case["verb"].as_str();
        let decisions = commitments_from_messages(&[Message::new("user", text)], "user", 7, 2026);
        if decisions.len() != expected_count {
            commitment_failures.push(format!(
                "`{text}` → {} decision(s), expected {expected_count}",
                decisions.len()
            ));
            continue;
        }
        if let (Some(verb), Some(decision)) = (expected_verb, decisions.first()) {
            if decision.verb != verb {
                commitment_failures.push(format!(
                    "`{text}` → verb `{}`, expected `{verb}`",
                    decision.verb
                ));
            }
        }
    }

    let must_pct = must_hit * 100 / must_total.max(1);
    let should_pct = should_hit * 100 / should_total.max(1);

    tracing::info!(
        must_catch = format!("{must_hit}/{must_total} ({must_pct}%)"),
        should_catch = format!("{should_hit}/{should_total} ({should_pct}%)"),
        phantom_lines = format!("{}/{ignore_total}", phantoms.len()),
        over_extracted = over_extracted.len(),
        "compile-yield report"
    );
    if !must_misses.is_empty() {
        tracing::info!(misses = ?must_misses, "must_catch misses (regressions)");
    }
    if !should_misses.is_empty() {
        tracing::info!(misses = ?should_misses, "should_catch misses (capability gaps)");
    }
    if !phantoms.is_empty() {
        tracing::info!(phantoms = ?phantoms, "phantom facts on filler");
    }
    if !over_extracted.is_empty() {
        tracing::info!(facts = ?over_extracted, "facts outside the annotated types");
    }
    if !commitment_failures.is_empty() {
        tracing::info!(failures = ?commitment_failures, "commitment extraction");
    }

    assert_eq!(
        negated_checked, 1,
        "the corpus must keep its negation sample, otherwise the invariant is not exercised"
    );
    assert!(
        phantoms.is_empty(),
        "filler and small talk must produce no facts, got {phantoms:?}"
    );
    assert!(
        must_pct >= MUST_CATCH_RECALL_PCT,
        "must_catch recall {must_pct}% dropped below the floor {MUST_CATCH_RECALL_PCT}%: {must_misses:?}"
    );
    assert!(
        should_pct >= SHOULD_CATCH_RECALL_PCT,
        "should_catch recall {should_pct}% dropped below the floor {SHOULD_CATCH_RECALL_PCT}%: {should_misses:?}"
    );
    assert!(
        commitment_failures.is_empty(),
        "commitment extraction must match the corpus: {commitment_failures:?}"
    );
}
