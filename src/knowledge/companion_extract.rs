//! Companion-signal extraction — the "你来我往" layer of the cognition
//! compiler.
//!
//! The statement markers in [`crate::agent_personality`] capture explicit
//! "我是…" / "我喜欢…" statements. But companion dialogs are mostly
//! **implicit** — emotions, self-descriptions, and recurring topics woven
//! into first-person narration. This module extracts those signals with
//! deterministic, evidence-anchored rules (no LLM):
//!
//! 1. `emotion_series` — lexicon hits (`config/emotion_lexicon.json`,
//!    classic + vernacular zones) tagged with turn index and the original
//!    quote, so the user's/agent's emotional trajectory is reconstructible.
//! 2. `self_cognition` — first-person self-descriptions ("我是不是太软弱",
//!    "我总是不敢拒绝") — the highest-value cognition a companion AI can
//!    store about its user.
//! 3. `repeated_themes` — topic keywords clustered across turns; what the
//!    user keeps coming back to matters more than what they said once.
//!
//! Zero-pollution: extraction is per-message with role preserved; nothing is
//! ever merged across roles, and every signal carries its quote (evidence).

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::agent_personality::AGENT_PERSONALITY_ATTRIBUTION;
use crate::cognition::{Fact, FactType};
use crate::types::Message;

/// Grayscale switch: `false` now means companion signals are wired into the
/// `agent_fact_compile` production chain (graduated from grayscale). Flip back
/// to `true` to revert to grayscale: extract signals but do not wire them.
pub const COMPANION_EXTRACT_GRAYSCALE: bool = false;

/// One emotion observation, evidence-anchored.
#[derive(Debug, Clone, PartialEq)]
pub struct EmotionSample {
    /// Canonical emotion label (from the lexicon), e.g. "委屈".
    pub label: String,
    /// Lexicon zone: "classic" or "vernacular".
    pub zone: String,
    /// Zero-based turn index in the dialog.
    pub turn: usize,
    /// Speaker role ("user" / "assistant").
    pub role: String,
    /// The original message text (evidence).
    pub quote: String,
}

/// A first-person self-description, evidence-anchored.
#[derive(Debug, Clone, PartialEq)]
pub struct SelfCognition {
    /// Speaker role.
    pub role: String,
    /// Zero-based turn index.
    pub turn: usize,
    /// The self-referential sentence (evidence).
    pub quote: String,
}

/// A recurring topic signal, evidence-anchored.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeSignal {
    /// The topic keyword.
    pub keyword: String,
    /// How many distinct turns mention it.
    pub occurrences: usize,
    /// Sample quotes (up to 3) as evidence.
    pub samples: Vec<String>,
}

/// Full companion-extraction result.
#[derive(Debug, Clone, Default)]
pub struct CompanionExtract {
    pub emotions: Vec<EmotionSample>,
    pub self_cognitions: Vec<SelfCognition>,
    pub themes: Vec<ThemeSignal>,
}

/// JSON shape of `config/emotion_lexicon.json`: zone → label → keywords.
#[derive(Debug, Clone, Deserialize)]
struct EmotionLexicon {
    #[serde(default)]
    classic: HashMap<String, Vec<String>>,
    #[serde(default)]
    vernacular: HashMap<String, Vec<String>>,
}

static LEXICON: LazyLock<EmotionLexicon> = LazyLock::new(|| {
    let path = crate::config::resolve_resource_path("config/emotion_lexicon.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| EmotionLexicon {
            classic: HashMap::new(),
            vernacular: HashMap::new(),
        })
});

/// First-person self-description prefixes ("我…" + stance/attribute verb).
const SELF_COGNITION_PATTERNS: &[&str] = &[
    "我是",
    "我总",
    "我老",
    "我其实",
    "我一直",
    "我从来",
    "我从来都",
    "我是不是",
    "我承认",
    "我最大的",
    "我这个人",
    "我天生",
    "我骨子里",
    "我忍不住",
    "我学不会",
    "我不敢",
    "我不肯",
    "我偏偏",
    "我到底",
    "我宁可",
    "我宁愿",
    "我居然",
];

/// Stop words never allowed as a theme keyword.
const THEME_STOP: &[&str] = &[
    "这个", "那个", "什么", "怎么", "就是", "不是", "没有", "自己", "我们", "你们", "他们", "因为",
    "所以", "但是", "如果", "还有", "知道", "觉得", "可以", "现在", "时候", "一个", "真的", "好像",
    "反正", "然后", "其实", "我", "你", "他", "她", "它", "这", "那", "，我", "。我", "我", "——",
    "了。", "的。", "，你", "吗", "呢", "么", "是", "了", "的", "在", "有", "就", "都", "也", "很",
    "太", "别", "再", "又", "还", "把", "被", "让", "给", "跟", "和", "与", "对", "从", "向", "到",
    "往", "于", "上", "下", "里", "中",
];

/// Run all three companion extractors over a message list.
#[must_use]
pub fn extract_companion_signals(messages: &[Message]) -> CompanionExtract {
    CompanionExtract {
        emotions: extract_emotion_series(messages),
        self_cognitions: extract_self_cognition(messages),
        themes: extract_repeated_themes(messages),
    }
}

/// Convert a [`CompanionExtract`] into two fact groups keyed by speaker role.
///
/// Returns `(user_facts, agent_facts)`:
/// - user-side signals (emotion / identity / preference) are attributed to
///   `user_entity_id` and carry NO attribution marker, preserving the
///   zero-pollution invariant of the `user_facts` channel.
/// - assistant-side signals are attributed to `agent_entity_id` and tagged
///   `attribution = "agent_personality"` so the persona layer can recognize them.
///
/// `messages` is needed only to resolve the speaker role of recurring themes
/// (a [`ThemeSignal`] carries no role of its own — its side is inferred from the
/// first sample quote).
#[must_use]
pub fn companion_facts_from_extract(
    extract: &CompanionExtract,
    messages: &[Message],
    user_entity_id: i64,
    agent_entity_id: i64,
    logical_time: i64,
) -> (Vec<Fact>, Vec<Fact>) {
    let mut user_facts = Vec::new();
    let mut agent_facts = Vec::new();

    for emotion in &extract.emotions {
        let fact = Fact {
            id: None,
            entity_id: if emotion.role == "user" {
                user_entity_id
            } else {
                agent_entity_id
            },
            fact_type: FactType::Emotion,
            time: logical_time,
            payload: serde_json::json!({
                "content": emotion.quote,
                "emotion": emotion.label,
                "zone": emotion.zone,
                "turn": emotion.turn,
                // Standard EvidenceRef so anchor_evidence_on writes a real
                // evidence row — without it provenance answers `null` for
                // every companion emotion fact.
                "evidence": {
                    "doc_id": 0,
                    "offset": 0,
                    "length": emotion.quote.chars().count().min(emotion.quote.len()),
                    "text": emotion.quote,
                },
            }),
            evidence_id: None,
            created_at: logical_time,
            ..Fact::default()
        };
        push_companion_fact(fact, &emotion.role, &mut user_facts, &mut agent_facts);
    }

    for cognition in &extract.self_cognitions {
        let fact = Fact {
            id: None,
            entity_id: if cognition.role == "user" {
                user_entity_id
            } else {
                agent_entity_id
            },
            fact_type: FactType::Identity,
            time: logical_time,
            payload: serde_json::json!({
                "content": cognition.quote,
                "turn": cognition.turn,
                "evidence": {
                    "doc_id": 0,
                    "offset": 0,
                    "length": cognition.quote.len(),
                    "text": cognition.quote,
                },
            }),
            evidence_id: None,
            created_at: logical_time,
            ..Fact::default()
        };
        push_companion_fact(fact, &cognition.role, &mut user_facts, &mut agent_facts);
    }

    for theme in &extract.themes {
        let role = theme_role(theme, messages);
        // Zero-pollution: keep only samples authored by the assigned role so
        // an assistant echo never lands in the user_facts payload.
        let samples: Vec<String> = theme
            .samples
            .iter()
            .filter(|s| {
                messages
                    .iter()
                    .any(|m| m.role == role && m.content.as_str() == s.as_str())
            })
            .cloned()
            .collect();
        let sample_text = samples.first().cloned().unwrap_or_default();
        let fact = Fact {
            id: None,
            entity_id: if role == "user" {
                user_entity_id
            } else {
                agent_entity_id
            },
            fact_type: FactType::Preference,
            time: logical_time,
            payload: serde_json::json!({
                "keyword": theme.keyword,
                "occurrences": theme.occurrences,
                "samples": samples,
                "evidence": {
                    "doc_id": 0,
                    "offset": 0,
                    "length": sample_text.len(),
                    "text": sample_text,
                },
            }),
            evidence_id: None,
            created_at: logical_time,
            ..Fact::default()
        };
        push_companion_fact(fact, role, &mut user_facts, &mut agent_facts);
    }

    (user_facts, agent_facts)
}

/// Route a companion fact to the user or agent channel by speaker role.
///
/// User-side facts keep the channel free of any attribution marker (the
/// zero-pollution invariant); assistant-side facts are tagged
/// `agent_personality` so the persona layer can recognize them.
/// System/tool/unknown roles are DROPPED: routing them to the agent channel
/// let a system prompt or tool dump containing "我是…" become a permanent
/// persona fact.
fn push_companion_fact(
    mut fact: Fact,
    role: &str,
    user_facts: &mut Vec<Fact>,
    agent_facts: &mut Vec<Fact>,
) {
    if role == "user" {
        user_facts.push(fact);
    } else if role == "assistant" {
        fact.payload["attribution"] =
            serde_json::Value::String(AGENT_PERSONALITY_ATTRIBUTION.to_string());
        agent_facts.push(fact);
    }
    // system / tool / other: drop (never enter either cognition channel).
}

/// Resolve the speaker role of a recurring theme from its first sample quote.
fn theme_role<'a>(theme: &ThemeSignal, messages: &'a [Message]) -> &'a str {
    let role = messages
        .iter()
        .find(|m| {
            theme
                .samples
                .first()
                .is_some_and(|sample| m.content.contains(sample))
        })
        .map(|m| m.role.as_str())
        .unwrap_or("user");
    // The returned slice borrows from a message, not from `theme`, so the
    // lifetime is tied to `messages` as the caller expects.
    role
}

/// Lexicon hits per message, tagged with turn/role/quote.
#[must_use]
pub fn extract_emotion_series(messages: &[Message]) -> Vec<EmotionSample> {
    let mut out = Vec::new();
    for (turn, msg) in messages.iter().enumerate() {
        for (zone, table) in [
            ("classic", &LEXICON.classic),
            ("vernacular", &LEXICON.vernacular),
        ] {
            for (label, keywords) in table {
                if keywords.iter().any(|k| msg.content.contains(k.as_str())) {
                    out.push(EmotionSample {
                        label: label.clone(),
                        zone: zone.to_string(),
                        turn,
                        role: msg.role.clone(),
                        quote: msg.content.clone(),
                    });
                    break; // one label per zone per message keeps the signal clean
                }
            }
        }
    }
    out
}

/// First-person self-description sentences, evidence-anchored.
#[must_use]
pub fn extract_self_cognition(messages: &[Message]) -> Vec<SelfCognition> {
    let mut out = Vec::new();
    for (turn, msg) in messages.iter().enumerate() {
        let has_self = SELF_COGNITION_PATTERNS
            .iter()
            .any(|p| msg.content.contains(p));
        if has_self {
            out.push(SelfCognition {
                role: msg.role.clone(),
                turn,
                quote: msg.content.clone(),
            });
        }
    }
    out
}

/// Single-character stop words distilled from [`THEME_STOP`]; a 2-char bigram
/// touching one of these straddles a function word ("操的", "我是", "这样")
/// and must not become a topic keyword.
static THEME_STOP_CHARS: LazyLock<std::collections::HashSet<char>> = LazyLock::new(|| {
    THEME_STOP
        .iter()
        .filter(|s| s.chars().count() == 1)
        .flat_map(|s| s.chars())
        .collect()
});

/// Topic keywords clustered across turns (>=2 turns → a "recurring theme").
///
/// Counting stays cross-role (a topic both speakers keep returning to IS
/// recurring for the relationship snapshot). Zero-pollution is enforced when
/// the theme becomes a fact: [`companion_facts_from_extract`] filters
/// `samples` down to messages of the assigned role so assistant text never
/// lands in the user_facts channel.
#[must_use]
pub fn extract_repeated_themes(messages: &[Message]) -> Vec<ThemeSignal> {
    let mut turn_count: HashMap<String, usize> = HashMap::new();
    let mut seen_turn: HashMap<String, usize> = HashMap::new();
    let mut samples: HashMap<String, Vec<String>> = HashMap::new();

    for (turn, msg) in messages.iter().enumerate() {
        let chars: Vec<char> = msg.content.chars().collect();
        let mut candidates: Vec<String> = Vec::new();
        for w in chars.windows(2) {
            if w[0].is_ascii_alphabetic() || w[1].is_ascii_alphabetic() {
                continue;
            }
            if !w[0].is_alphanumeric() || !w[1].is_alphanumeric() {
                continue;
            }
            if THEME_STOP_CHARS.contains(&w[0]) || THEME_STOP_CHARS.contains(&w[1]) {
                continue;
            }
            let kw: String = w.iter().collect();
            if kw.is_empty() || THEME_STOP.contains(&kw.as_str()) {
                continue;
            }
            candidates.push(kw);
        }
        candidates.sort();
        candidates.dedup();
        for kw in candidates {
            let prev = seen_turn.entry(kw.clone()).or_insert(usize::MAX);
            if *prev != turn {
                *turn_count.entry(kw.clone()).or_default() += 1;
                *prev = turn;
            }
            let s = samples.entry(kw.clone()).or_default();
            if s.len() < 3 {
                s.push(msg.content.clone());
            }
        }
    }

    let mut out: Vec<ThemeSignal> = turn_count
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(keyword, occurrences)| {
            let samples = samples.remove(&keyword).unwrap_or_default();
            ThemeSignal {
                keyword,
                occurrences,
                samples,
            }
        })
        .collect();
    // Deterministic total order: occurrence DESC, then keyword ASC — HashMap
    // into_iter is process-random, so occurrence-only sort left ties in hash
    // order and different runs produced different recent_topics.
    out.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then_with(|| a.keyword.cmp(&b.keyword))
    });
    out.truncate(20);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(pairs: &[(&str, &str)]) -> Vec<Message> {
        pairs.iter().map(|(r, c)| Message::new(*r, *c)).collect()
    }

    /// Objective: Verify emotion lexicon hits are evidence-anchored and
    /// role/turn-tagged.
    /// Invariants: "怕" in user turn 1 → EmotionSample(害怕, turn 1, role user).
    #[test]
    fn emotion_series_tags_evidence() {
        let m = msgs(&[
            ("user", "今天被老板骂了，烦死了。"),
            ("assistant", "我夜里睡不着，心里发慌。"),
        ]);
        let e = extract_emotion_series(&m);
        assert!(
            e.iter()
                .any(|s| s.label == "烦" && s.role == "user" && s.turn == 0),
            "user 烦 must be tagged, got {e:?}"
        );
        assert!(
            e.iter()
                .any(|s| s.zone == "classic" && s.label == "害怕" && s.turn == 1),
            "classic 害怕 must hit, got {e:?}"
        );
        // Every sample must carry its original quote (evidence).
        assert!(
            e.iter().all(|s| !s.quote.is_empty()),
            "every emotion sample needs evidence"
        );
    }

    /// Objective: Verify first-person self-descriptions are caught.
    /// Invariants: "我是不是太软弱" → SelfCognition with the quote.
    #[test]
    fn self_cognition_catches_first_person() {
        let m = msgs(&[("user", "你说我是不是太软弱，每次都忍")]);
        let s = extract_self_cognition(&m);
        assert_eq!(s.len(), 1, "one self-cognition expected");
        assert!(s[0].quote.contains("我是不是太软弱"), "quote preserved");
    }

    /// Objective: Verify recurring themes need >=2 distinct turns.
    /// Invariants: 工作 appears in 3 turns → theme; one-off word absent.
    #[test]
    fn repeated_themes_require_two_turns() {
        let m = msgs(&[
            ("user", "工作的事烦死了"),
            ("user", "工作又压了一堆"),
            ("assistant", "你工作别太拼了"),
            ("user", "今天天气不错"),
        ]);
        let t = extract_repeated_themes(&m);
        assert!(
            t.iter().any(|s| s.keyword == "工作" && s.occurrences >= 2),
            "工作 must recur, got {t:?}"
        );
        assert!(
            t.iter().all(|s| s.keyword != "天气"),
            "one-off word must not be a theme"
        );
    }

    /// Objective: Verify punctuation/digit fragments never become themes.
    /// Invariants: "0.", ".9", "2.", "吧。" (from "54M 是不是有点大啊？")
    /// and "，不" must all be rejected as topic keywords.
    #[test]
    fn punctuation_fragments_are_rejected() {
        let m = msgs(&[
            ("user", "54M 是不是有点大啊？"),
            ("assistant", "加 -ldflags 从 54M 降到 33.5M。"),
        ]);
        let t = extract_repeated_themes(&m);
        for noise in ["0.", ".9", "2.", ".2", "吧。", "，不", "级到", "是曹"] {
            assert!(
                t.iter().all(|s| s.keyword != noise),
                "fragment `{noise}` must not be a theme, got {t:?}"
            );
        }
        // The meaningful bigram (54) survives as a topic.
        assert!(
            t.iter().any(|s| s.keyword == "54"),
            "54 must survive filtering, got {t:?}"
        );
    }

    /// Objective: Verify bigrams straddling a single-char stop word are
    /// rejected — the "操的/我是/他的" class of fragmented keywords.
    /// Invariants: none of the stop-word straddlers appear; real words like
    /// 曹操 and 欣赏 still recur.
    #[test]
    fn stop_word_straddlers_are_rejected() {
        let m = msgs(&[
            ("user", "我喜欢曹操，欣赏他的雄才大略"),
            ("assistant", "我是曹操的推崇者，他的用人不拘一格"),
            ("user", "曹操的知人善任，特别欣赏"),
        ]);
        let t = extract_repeated_themes(&m);
        for noise in ["操的", "我是", "他的", "我特", "这样", "赏曹", "样的"] {
            assert!(
                t.iter().all(|s| s.keyword != noise),
                "straddler `{noise}` must not be a theme, got {t:?}"
            );
        }
        assert!(
            t.iter().any(|s| s.keyword == "曹操"),
            "曹操 must survive filtering, got {t:?}"
        );
    }

    /// Objective: Verify an empty message list produces no themes (no panic).
    /// Invariants: empty input → empty output.
    #[test]
    fn empty_messages_produce_no_themes() {
        let t = extract_repeated_themes(&[]);
        assert!(t.is_empty(), "no messages → no themes");
    }

    /// Objective: Verify a single message (one turn) never forms a theme.
    /// Invariants: recurring requires >=2 distinct turns, even when a word
    /// repeats inside one long message.
    #[test]
    fn single_turn_repeats_do_not_form_theme() {
        let m = msgs(&[("user", "曹操曹操曹操，都是曹操")]);
        let t = extract_repeated_themes(&m);
        assert!(
            t.iter().all(|s| s.keyword != "曹操"),
            "same-turn repeats must not count as recurring, got {t:?}"
        );
    }

    /// Objective: Verify the grayscale switch is now connected to production.
    /// Invariants: companion signals feed `agent_fact_compile`, so the switch
    /// is `false`.
    #[test]
    fn grayscale_switch_connected_to_production() {
        let connected = !COMPANION_EXTRACT_GRAYSCALE;
        assert!(connected, "companion signals are connected to production");
    }
}
