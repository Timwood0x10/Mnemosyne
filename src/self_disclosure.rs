//! Self-disclosure extraction — the "who is this person?" channel.
//!
//! The observation marker table answers "what does the user feel / want /
//! dislike", and it can only ever produce preference / goal / emotion / event.
//! Everything a companion needs in order to know *who* it is talking to —
//! "我叫小林，26 岁，在杭州做后端开发", "我养了一只猫", "平时会去爬山" — was
//! therefore invisible: recall on those lines measured **0%**
//! (see `docs/zh/compile-quality.md`).
//!
//! Rule-driven and LLM-free, like every other extractor in this engine. Each rule
//! pairs an explicit cue ("我叫", "我养了", "我在…做…") with a conservative value:
//! either a closed list (family terms, role suffixes) or the remainder of its
//! clause. A rule that cannot produce a non-empty value produces nothing — this
//! channel never guesses.
//!
//! Facts are typed so they land inside the frozen five-dimension model:
//!
//! | disclosure | type | payload key |
//! |---|---|---|
//! | 名字 / 年龄 / 职业 / 城市 | [`FactType::Identity`] | `attribute` = name / age / occupation / location |
//! | 家人 / 宠物 | [`FactType::Relationship`] | `target` |
//! | 爱好 | [`FactType::Interest`] | `content` |
//! | 习惯 | [`FactType::Habit`] | `content` |
//!
//! Two deliberate refusals:
//!
//! - A **negated** clause is skipped instead of being stored with a flag. The
//!   disclosure types have no negation-aware consumer, and "我不是医生" is not
//!   worth a fact.
//! - A cue whose value fails validation (a role that does not look like a role, a
//!   pet that would be a whole sentence) yields nothing, so "我是说真的" never
//!   becomes an occupation.

use crate::cognition::{Fact, FactType};
use crate::types::Message;

/// Clause terminators: a disclosure never crosses one.
///
/// Whitespace is deliberately NOT a terminator (it would split "26 岁"), values
/// simply stop at the first space — see [`after_cue`].
const CLAUSE_BREAKS: &[char] = &[
    '，', '。', '！', '？', '；', '、', '：', ',', '.', '!', '?', ';', ':',
];

/// Cues that cancel a disclosure instead of qualifying it.
///
/// Multi-character on purpose: bare `不`/`非` matched inside ordinary words
/// (`不错`, `非常`, `南非`) and silently dropped valid disclosures such as
/// "我在南非工作" or "我叫小林不错".
const NEGATION_CUES: &[&str] = &[
    "不是",
    "不在",
    "不能",
    "不会",
    "不要",
    "不想",
    "不喜欢",
    "没有",
    "没在",
    "别",
    "未",
];

/// Family terms a disclosure may name. Closed on purpose — matching freely would
/// manufacture relatives.
const FAMILY_TERMS: &[&str] = &[
    "妈妈",
    "妈",
    "爸爸",
    "爸",
    "哥哥",
    "哥",
    "姐姐",
    "姐",
    "弟弟",
    "弟",
    "妹妹",
    "妹",
    "儿子",
    "女儿",
    "老公",
    "老婆",
    "丈夫",
    "妻子",
    "男朋友",
    "女朋友",
    "男友",
    "女友",
    "爷爷",
    "奶奶",
    "外公",
    "外婆",
    "姥姥",
    "姥爷",
    "叔叔",
    "阿姨",
    "舅舅",
    "姑姑",
];

/// A role has to end with one of these: "我是说真的" must not become an
/// occupation.
const ROLE_SUFFIXES: &[&str] = &[
    "工程师",
    "设计师",
    "程序员",
    "开发",
    "老师",
    "医生",
    "护士",
    "经理",
    "运营",
    "销售",
    "律师",
    "会计",
    "公务员",
    "学生",
    "研究生",
    "博士生",
    "创业者",
];

/// Quantifiers stripped from a possession ("我养了一只猫" → "猫").
const QUANTIFIERS: &[&str] = &["一只", "一个", "一条", "一位", "只", "个", "条", "位"];

/// Verbs that separate "where" from "what" in "我在杭州做后端开发".
const CRAFT_VERBS: &[&str] = &["做", "从事", "干"];

/// Verbs that end a location disclosure ("我在杭州工作").
const PLACE_VERBS: &[&str] = &["工作", "上班", "生活", "读书", "上学", "住"];

/// Cues introducing a name.
const NAME_CUES: &[&str] = &["我的名字是", "我的名字叫", "我名字叫", "我叫", "我姓"];

/// Cues introducing an interest.
const INTEREST_CUES: &[&str] = &["我的爱好是", "爱好是", "平时会去", "我常去", "我很喜欢去"];

/// Cues introducing a habit.
const HABIT_CUES: &[&str] = &["我每天", "我每周", "我习惯", "我经常", "每天", "每周"];

/// Cues introducing a possession ("我养了一只猫", "我有一条狗").
const POSSESSION_CUES: &[&str] = &["我养了", "我有一只", "我有一个", "我有一条"];

/// The longest value a disclosure may contribute; longer text is a sentence, not
/// a value.
const MAX_VALUE_CHARS: usize = 16;

/// Extract self-disclosure facts from the user's messages.
#[must_use]
pub fn disclosures_from_messages(messages: &[Message], entity_id: i64, time: i32) -> Vec<Fact> {
    if entity_id <= 0 {
        return Vec::new();
    }
    let mut facts = Vec::new();
    for message in messages.iter().filter(|message| message.is_user()) {
        for clause in clauses(&message.content) {
            if NEGATION_CUES.iter().any(|cue| clause.text.contains(cue)) {
                continue;
            }
            collect(&clause, &message.content, entity_id, time, &mut facts);
        }
    }
    facts
}

/// One clause of a message and the byte offset it starts at.
struct Clause<'a> {
    text: &'a str,
    start: usize,
}

/// Split a message into clauses, keeping each byte offset so evidence anchors
/// stay exact.
fn clauses(message: &str) -> Vec<Clause<'_>> {
    let mut clauses = Vec::new();
    let mut start = 0usize;
    for (index, character) in message.char_indices() {
        if CLAUSE_BREAKS.contains(&character) {
            if index > start {
                clauses.push(Clause {
                    text: &message[start..index],
                    start,
                });
            }
            start = index + character.len_utf8();
        }
    }
    if start < message.len() {
        clauses.push(Clause {
            text: &message[start..],
            start,
        });
    }
    clauses
}

/// Apply every disclosure rule to one clause.
fn collect(clause: &Clause<'_>, message: &str, entity_id: i64, time: i32, facts: &mut Vec<Fact>) {
    let text = clause.text;

    if let Some((value, at)) = after_cue(text, NAME_CUES) {
        push_identity(
            facts,
            "name",
            value,
            &Anchor::new(clause, at, NAME_CUES),
            message,
            entity_id,
            time,
        );
    }
    if let Some((age, at)) = age_of(text) {
        push_identity(
            facts,
            "age",
            &age,
            &Anchor::new(clause, at, &[]),
            message,
            entity_id,
            time,
        );
    }
    if let Some((place, craft, at)) = place_and_craft(text) {
        push_identity(
            facts,
            "location",
            place,
            &Anchor::new(clause, at, &[]),
            message,
            entity_id,
            time,
        );
        if let Some(occupation) = craft.and_then(normalise_role) {
            push_identity(
                facts,
                "occupation",
                &occupation,
                &Anchor::new(clause, at, &[]),
                message,
                entity_id,
                time,
            );
        }
    } else if let Some((value, at)) = after_cue(text, &["我是", "我做", "我从事"]) {
        if let Some(occupation) = normalise_role(value) {
            push_identity(
                facts,
                "occupation",
                &occupation,
                &Anchor::new(clause, at, &[]),
                message,
                entity_id,
                time,
            );
        }
    }
    if let Some(term) = FAMILY_TERMS.iter().find(|term| text.contains(**term)) {
        push_relationship(
            facts,
            term,
            text,
            &Anchor::new(clause, text.find(term).unwrap_or(0), &[term]),
            message,
            entity_id,
            time,
        );
    } else if let Some((value, at)) = after_cue(text, POSSESSION_CUES) {
        if let Some(pet) = normalise_possession(value) {
            push_relationship(
                facts,
                &pet,
                text,
                &Anchor::new(clause, at, POSSESSION_CUES),
                message,
                entity_id,
                time,
            );
        }
    }
    if let Some((value, at)) = after_cue(text, INTEREST_CUES) {
        push_plain(
            facts,
            FactType::Interest,
            value,
            &Anchor::new(clause, at, INTEREST_CUES),
            message,
            entity_id,
            time,
        );
    }
    if let Some((value, at)) = after_cue(text, HABIT_CUES) {
        push_plain(
            facts,
            FactType::Habit,
            value,
            &Anchor::new(clause, at, HABIT_CUES),
            message,
            entity_id,
            time,
        );
    }
}

/// Where a fact came from: the message, the byte offset of its cue, and the
/// matched marker for the `length` field.
struct Anchor {
    offset: usize,
    length: usize,
}

impl Anchor {
    /// Build an anchor from a cue match inside `clause`.
    fn new(clause: &Clause<'_>, at: usize, cues: &[&str]) -> Self {
        let matched = cues
            .iter()
            .find(|cue| clause.text[at..].starts_with(**cue))
            .map_or(0, |cue| cue.len());
        Anchor {
            offset: clause.start + at,
            length: matched.max(1),
        }
    }
}

/// Read the value that follows the first matching cue.
///
/// The value ends at the clause end or at the first whitespace, so
/// "我叫小林 26岁" discloses the name `小林` rather than the whole tail.
fn after_cue<'a>(text: &'a str, cues: &[&str]) -> Option<(&'a str, usize)> {
    for cue in cues {
        if let Some(at) = text.find(cue) {
            let value = text[at + cue.len()..]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim();
            if !value.is_empty() && value.chars().count() <= MAX_VALUE_CHARS {
                return Some((value, at));
            }
        }
    }
    None
}

/// Read the digits immediately before "岁" (whitespace allowed), with the byte
/// offset they start at.
fn age_of(text: &str) -> Option<(String, usize)> {
    let at = text.find('岁')?;
    // Digits are taken from the TRIMMED prefix, so the anchor must be
    // `trimmed_len - digits_len` — not `at - digits_len`. With "26 岁",
    // `at` points past the space, and `at - digits.len()` landed on `'6'`
    // instead of `'2'`.
    let trimmed = text[..at].trim_end();
    let digits: String = trimmed
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if digits.is_empty() || digits.chars().count() > 3 {
        return None;
    }
    Some((digits.clone(), trimmed.len() - digits.len()))
}

/// Split "我在杭州做后端开发" / "在杭州工作" into `(place, craft, offset)`.
///
/// The bare "在" form exists because Chinese elides the subject across clauses:
/// "我叫小林，在杭州做后端开发" puts the 我 in the previous clause, so a rule that
/// demanded "我在" would lose the city and the job.
fn place_and_craft(text: &str) -> Option<(&str, Option<&str>, usize)> {
    let (at, rest) = match text.find("我在") {
        Some(at) => (at, &text[at + "我在".len()..]),
        // A sub-clause may start with 在 and inherit its subject.
        None => (0, text.strip_prefix('在')?),
    };
    let mut split: Option<(usize, bool)> = None;
    for verb in CRAFT_VERBS {
        if let Some(index) = rest.find(verb)
            && split.is_none_or(|(best, _)| index < best)
        {
            split = Some((index, true));
        }
    }
    for verb in PLACE_VERBS {
        if let Some(index) = rest.find(verb)
            && split.is_none_or(|(best, _)| index < best)
        {
            split = Some((index, false));
        }
    }
    let (index, is_craft) = split?;
    let place = rest[..index].trim();
    if place.is_empty() || place.chars().count() > MAX_VALUE_CHARS {
        return None;
    }
    let craft = if is_craft {
        let verb = CRAFT_VERBS
            .iter()
            .find(|verb| rest[index..].starts_with(**verb))?;
        Some(rest[index + verb.len()..].trim())
    } else {
        None
    };
    Some((place, craft, at))
}

/// Accept a role only when it ends with a known suffix (after stripping a
/// leading craft verb and a trailing 的).
fn normalise_role(value: &str) -> Option<String> {
    let mut role = value.trim().trim_end_matches('的').trim();
    for verb in CRAFT_VERBS {
        if let Some(rest) = role.strip_prefix(verb) {
            role = rest.trim();
        }
    }
    if role.is_empty() || role.chars().count() > MAX_VALUE_CHARS {
        return None;
    }
    ROLE_SUFFIXES
        .iter()
        .any(|suffix| role.ends_with(suffix))
        .then(|| role.to_string())
}

/// Strip a quantifier from a possession ("一只猫" → "猫").
fn normalise_possession(value: &str) -> Option<String> {
    let mut noun = value.trim().trim_end_matches('，').trim();
    for quantifier in QUANTIFIERS {
        if let Some(rest) = noun.strip_prefix(quantifier) {
            noun = rest.trim();
            break;
        }
    }
    if noun.is_empty() || noun.chars().count() > MAX_VALUE_CHARS {
        return None;
    }
    Some(noun.to_string())
}

/// Build an Identity fact (`attribute` decides the current-state bucket).
fn push_identity(
    facts: &mut Vec<Fact>,
    attribute: &str,
    value: &str,
    anchor: &Anchor,
    message: &str,
    entity_id: i64,
    time: i32,
) {
    push(
        facts,
        FactType::Identity,
        serde_json::json!({ "attribute": attribute, "content": value }),
        anchor,
        message,
        entity_id,
        time,
    );
}

/// Build a Relationship fact (`target` decides the current-state bucket).
fn push_relationship(
    facts: &mut Vec<Fact>,
    target: &str,
    content: &str,
    anchor: &Anchor,
    message: &str,
    entity_id: i64,
    time: i32,
) {
    push(
        facts,
        FactType::Relationship,
        serde_json::json!({ "target": target, "content": content }),
        anchor,
        message,
        entity_id,
        time,
    );
}

/// Build an Interest / Habit fact.
fn push_plain(
    facts: &mut Vec<Fact>,
    fact_type: FactType,
    value: &str,
    anchor: &Anchor,
    message: &str,
    entity_id: i64,
    time: i32,
) {
    push(
        facts,
        fact_type,
        serde_json::json!({ "content": value }),
        anchor,
        message,
        entity_id,
        time,
    );
}

/// Assemble the fact: payload value plus the sentence and evidence anchor that
/// make it auditable.
fn push(
    facts: &mut Vec<Fact>,
    fact_type: FactType,
    mut payload: serde_json::Value,
    anchor: &Anchor,
    message: &str,
    entity_id: i64,
    time: i32,
) {
    payload["kind"] = serde_json::Value::from("self_disclosure");
    payload["evidence"] = serde_json::json!({
        "doc_id": 0,
        "offset": anchor.offset,
        "length": anchor.length,
        "text": message,
    });
    facts.push(Fact {
        id: None,
        entity_id,
        fact_type,
        time,
        payload,
        evidence_id: None,
        created_at: i64::from(time),
        ..Fact::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disclosures(text: &str) -> Vec<Fact> {
        disclosures_from_messages(&[Message::new("user", text)], 7, 2026)
    }

    /// Objective: Verify a self-introduction yields the identity facts a
    /// companion needs, keyed by `attribute` so the current-state view can bucket
    /// them (name / age / occupation / location). These types were unreachable
    /// before: the marker table can only emit preference/goal/emotion/event.
    /// Invariants: one fact per attribute, each carrying the sentence as evidence.
    #[test]
    fn self_introduction_yields_identity_attributes() {
        let facts = disclosures("你好呀，我叫小林，26 岁，在杭州做后端开发");

        let attribute = |name: &str| {
            facts
                .iter()
                .find(|fact| fact.payload["attribute"] == name)
                .unwrap_or_else(|| panic!("missing `{name}` disclosure: {facts:?}"))
        };
        assert_eq!(attribute("name").payload["content"], "小林");
        assert_eq!(attribute("age").payload["content"], "26");
        assert_eq!(attribute("location").payload["content"], "杭州");
        assert_eq!(attribute("occupation").payload["content"], "后端开发");
        for fact in &facts {
            assert_eq!(fact.fact_type, FactType::Identity);
            assert_eq!(
                fact.payload["evidence"]["text"],
                "你好呀，我叫小林，26 岁，在杭州做后端开发"
            );
            assert!(fact.payload["evidence"]["length"].as_u64().unwrap_or(0) > 0);
        }
    }

    /// Objective: Verify family and pet disclosures become Relationship facts
    /// bucketed by `target`, which is what `StateEngine::aggregate` reads.
    /// Invariants: a family term and a possession both land, with the quantifier
    /// stripped from the pet.
    #[test]
    fn relatives_and_pets_become_relationships() {
        let family = disclosures("我妈最近总催我相亲");
        assert_eq!(family.len(), 1, "one relation, got {family:?}");
        assert_eq!(family[0].fact_type, FactType::Relationship);
        assert_eq!(family[0].payload["target"], "妈");

        let pet = disclosures("我养了一只猫");
        assert_eq!(pet.len(), 1, "one relation, got {pet:?}");
        assert_eq!(pet[0].payload["target"], "猫");
    }

    /// Objective: Verify interests and habits are captured, and that a cue with
    /// nothing after it produces no fact — the channel never invents a value.
    /// Invariants: "平时会去爬山" yields an Interest; the trailing "算是为数不多的
    /// 爱好" clause yields nothing.
    #[test]
    fn interests_and_habits_need_a_value() {
        let interest = disclosures("平时会去爬山，算是为数不多的爱好");
        assert_eq!(interest.len(), 1, "one interest, got {interest:?}");
        assert_eq!(interest[0].fact_type, FactType::Interest);
        assert_eq!(interest[0].payload["content"], "爬山");

        let habit = disclosures("每天都要喝两杯咖啡");
        assert_eq!(habit.len(), 1, "one habit, got {habit:?}");
        assert_eq!(habit[0].fact_type, FactType::Habit);
    }

    /// Objective: Verify the channel refuses to guess. A role must look like a
    /// role and a disclosure must not be negated, otherwise small talk becomes
    /// identity data.
    /// Invariants: "我是说真的" / "我不是学生" / filler produce nothing.
    #[test]
    fn guessing_is_refused() {
        for text in [
            "我是说真的",
            "我不是学生",
            "你叫什么名字",
            "在吗",
            "嗯嗯，好的",
            "我先去吃饭了",
        ] {
            let facts = disclosures(text);
            assert!(
                facts.is_empty(),
                "`{text}` must yield nothing, got {facts:?}"
            );
        }
    }

    /// Objective: Verify the evidence offset points at the cue inside the message,
    /// so a reader can locate the disclosure in the original utterance.
    /// Invariants: offset is a char boundary and the marker matches the message.
    #[test]
    fn evidence_offset_locates_the_cue() {
        let message = "嗯，我叫小林";
        let facts = disclosures(message);
        assert_eq!(facts.len(), 1, "one disclosure, got {facts:?}");
        let offset = facts[0].payload["evidence"]["offset"]
            .as_u64()
            .expect("an offset") as usize;
        let length = facts[0].payload["evidence"]["length"]
            .as_u64()
            .expect("a length") as usize;
        assert!(
            message.is_char_boundary(offset) && message.is_char_boundary(offset + length),
            "the anchor must sit on char boundaries"
        );
        assert_eq!(
            &message[offset..offset + length],
            "我叫",
            "the anchor must point at the matched cue"
        );
    }
}
