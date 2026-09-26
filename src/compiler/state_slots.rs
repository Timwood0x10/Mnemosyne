//! Character-state slot extraction from Pass 2 events.
//!
//! Deterministic rules (no LLM): an action event's verb decides which
//! participant — if any — flips a state slot. Only *phrase-level* death
//! signals are used (mirrors the `ingest::extract::DEATH_KW` fix: bare
//! single characters like 死/杀 inside longer words mis-fire).
//!
//! ## Rules (v1)
//!
//! | Event shape | Slot | Value | Who |
//! |---|---|---|---|
//! | action, transitive-kill verb (杀/斩/…) with an object | `status` | `deceased` | **object** |
//! | action, intransitive-death verb (死/亡/薨/殁) | `status` | `deceased` | **subject** |
//! | dialogue / anything else | — | — | nobody |
//!
//! A **source-context guard** then rejects matches where the single-char verb
//! is only a substring of a longer non-terminal word (死守 / 杀出 / 誓死) —
//! the same bare-character defect class the ingest `DEATH_KW` fix removed.
//!
//! Each slot carries the event's narrative chapter and source byte span so
//! the state row is evidence-traceable (same contract as `events`).

use crate::compiler::Event;

/// A single character-state observation extracted from one event.
///
/// `chapter` / `start_offset` / `end_offset` come from the source event —
/// a slot row is only as locatable as the event that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct StateSlot {
    pub entity: String,
    pub slot: String,
    pub value: String,
    pub chapter: Option<i32>,
    pub start_offset: Option<i64>,
    pub end_offset: Option<i64>,
    pub confidence: f64,
}

/// Transitive kill verbs: the OBJECT of the event dies.
///
/// Single characters here are safe only WITH the context guard below: Pass 2
/// matches them as AhoCorasick patterns anywhere in the sentence (e.g. 杀 inside
/// 杀出重围), so the surrounding source characters decide whether the match is
/// a real kill. English forms are listed so the bilingual pipeline config
/// yields state flips too; the progressive `killing` is deliberately excluded
/// (the guard requires the full source word to be a listed form).
const KILL_VERBS: &[&str] = &[
    "杀", "斩", "弑", "诛", "鸩", "戮", "枭", "kill", "kills", "killed",
];

/// Intransitive death verbs: the SUBJECT of the event dies.
///
/// Progressive `dying` is deliberately excluded for the same reason as
/// `killing` above.
const DEATH_VERBS: &[&str] = &["死", "亡", "薨", "殁", "卒", "die", "dies", "died"];

/// Chars that may FOLLOW the matched death verb while the subject has NOT
/// died: 死守 / 死战 / 死拼 / 殊死搏 … (before-set handles 誓死 / 拼死).
/// Note 死了 / 死于 / 死者 / 死后 are deliberately absent — those are terminal.
const DEATH_AFTER: &[char] = &[
    '守', '战', '拼', '搏', '斗', '缠', '存', '缓', '硬', '苦', '力', '持', '撑', '咬', '盯', '赖',
    '活', '要', '心', '性', '地', '命', '罪',
];

/// Chars that may PRECEDE the matched death verb while it is non-terminal
/// (誓死 / 拼死 / 气死 / 笑死 …). 战死/病死/败死 are deliberately absent —
/// those ARE terminal readings.
const DEATH_BEFORE: &[char] = &[
    '誓', '殊', '决', '拼', '恨', '气', '烦', '笑', '爱', '怒', '恼', '狠', '急', '惊',
];

/// Chars that may FOLLOW the matched kill verb while nobody dies:
/// 杀出 / 杀进 / 杀回 / 杀散 / 杀伤 …. 杀了/杀死/杀于 are terminal (absent).
const KILL_AFTER: &[char] = &['出', '进', '回', '散', '伤'];

/// True when the matched verb at `event`'s span is embedded in a longer
/// word whose reading does NOT flip status.
///
/// English (ASCII) verbs expand the span to the whole alphabetic word and
/// require that word to be a listed form — "killed" fires, "killing" /
/// "skills" / "diesel" do not. Chinese verbs use the neighbor-char tables.
///
/// Returns `false` (guard disabled → title-only check) when offsets are
/// missing or `source` is empty/unusable — production always passes the
/// document text together with T6-stamped spans, so the guard is active there.
fn nonterminal_compound(event: &Event, verb: &str, source: &str) -> bool {
    let (Some(start), Some(end)) = (event.start_offset, event.end_offset) else {
        return false;
    };
    if source.is_empty()
        || start > end
        || end > source.len()
        || !source.is_char_boundary(start)
        || !source.is_char_boundary(end)
        || &source[start..end] != verb
    {
        return false;
    }
    if verb.is_ascii() {
        let bytes = source.as_bytes();
        let mut s = start;
        let mut e = end;
        while s > 0 && bytes[s - 1].is_ascii_alphabetic() {
            s -= 1;
        }
        while e < bytes.len() && bytes[e].is_ascii_alphabetic() {
            e += 1;
        }
        let word = source[s..e].to_ascii_lowercase();
        return !(KILL_VERBS.contains(&word.as_str()) || DEATH_VERBS.contains(&word.as_str()));
    }
    let after = source[end..].chars().next();
    let before = source[..start].chars().next_back();
    if KILL_VERBS.contains(&verb) {
        KILL_AFTER.contains(&after.unwrap_or('\0'))
    } else {
        // Death verbs: either side continuing the word makes the reading
        // non-terminal (死守: after=守; 誓死: before=誓).
        DEATH_AFTER.contains(&after.unwrap_or('\0'))
            || DEATH_BEFORE.contains(&before.unwrap_or('\0'))
    }
}

/// Extract every state-slot flip implied by one Pass 2 event.
///
/// `source` is the original document text the event's byte span points into;
/// pass `""` to disable the context guard (unit tests only — production must
/// pass the real text or embedded-verb false positives return).
///
/// Returns an empty vec for dialogue events and for actions that change no
/// tracked slot — callers must not assume one event yields one slot.
pub fn extract_state_slots(event: &Event, source: &str) -> Vec<StateSlot> {
    if event.event_type != "action" {
        return Vec::new();
    }
    // Title format from `extract::compile` is "subj verb" or "subj verb obj",
    // but entity names may contain spaces ("Mr. Smith kills") — a fixed token
    // index would pick a name fragment as the verb. Take the first whitespace
    // token that IS a tracked kill/death verb instead.
    let verb = event
        .title
        .split_whitespace()
        .find(|t| KILL_VERBS.contains(t) || DEATH_VERBS.contains(t));
    let Some(verb) = verb else {
        return Vec::new();
    };

    let (victim_role, confidence) = if KILL_VERBS.contains(&verb) {
        ("object", 0.75)
    } else {
        // find() above guarantees the token is in DEATH_VERBS.
        ("subject", 0.7)
    };

    // Reject substring-of-longer-word matches BEFORE picking a victim so a
    // 死守 sentence can never mark its subject deceased.
    if nonterminal_compound(event, verb, source) {
        return Vec::new();
    }

    let Some(victim) = event
        .participants
        .iter()
        .find(|p| p.role == victim_role)
        .map(|p| p.entity_name.clone())
    else {
        return Vec::new();
    };

    vec![StateSlot {
        entity: victim,
        slot: "status".into(),
        value: "deceased".into(),
        chapter: event.timestamp,
        start_offset: event.start_offset.map(|v| v as i64),
        end_offset: event.end_offset.map(|v| v as i64),
        confidence,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{Event, EventParticipant};

    /// Build an action event whose span covers `verb` inside `source`, so the
    /// context guard sees the same text production would.
    ///
    /// Verb lookup mirrors production: first title token that is a tracked
    /// kill/death verb (multi-word names like "Mr. Smith" shift positions),
    /// falling back to the second token for untracked verbs (救 …).
    fn action_event(title: &str, subj: &str, obj: Option<&str>, source: &str) -> Event {
        let verb = title
            .split_whitespace()
            .find(|t| KILL_VERBS.contains(t) || DEATH_VERBS.contains(t))
            .or_else(|| title.split_whitespace().nth(1))
            .expect("title must carry a verb");
        let at = source
            .find(verb)
            .unwrap_or_else(|| panic!("verb `{verb}` must occur in source"));
        let mut participants = vec![EventParticipant {
            entity_name: subj.into(),
            role: "subject".into(),
        }];
        if let Some(o) = obj {
            participants.push(EventParticipant {
                entity_name: o.into(),
                role: "object".into(),
            });
        }
        Event {
            id: None,
            title: title.into(),
            event_type: "action".into(),
            timestamp: Some(3),
            location: None,
            description: String::new(),
            participants,
            effects: vec![],
            importance: 0.6,
            start_offset: Some(at),
            end_offset: Some(at + verb.len()),
        }
    }

    /// Objective: Verify a transitive kill marks the OBJECT deceased and
    /// carries the event's chapter + span.
    /// Invariants: one slot; entity == object; status=deceased; offsets match.
    #[test]
    fn kill_verb_marks_object_deceased() {
        let src = "吕布杀董卓。";
        let slots = extract_state_slots(
            &action_event("吕布 杀 董卓", "吕布", Some("董卓"), src),
            src,
        );
        assert_eq!(
            slots.len(),
            1,
            "kill yields exactly one slot, got {slots:?}"
        );
        let s = &slots[0];
        assert_eq!(s.entity, "董卓", "victim is the object, not the killer");
        assert_eq!(s.slot, "status");
        assert_eq!(s.value, "deceased");
        assert_eq!(s.chapter, Some(3), "chapter copied from the event");
        assert_eq!(
            s.start_offset,
            Some(src.find("杀").expect("verb pos") as i64)
        );
        assert_eq!(
            s.end_offset,
            Some(src.find("杀").expect("verb pos") as i64 + "杀".len() as i64)
        );
    }

    /// Objective: Verify an intransitive death marks the SUBJECT deceased.
    /// Invariants: one slot; entity == subject.
    #[test]
    fn death_verb_marks_subject_deceased() {
        let src = "董卓死于渭水。";
        let slots = extract_state_slots(&action_event("董卓 死", "董卓", None, src), src);
        assert_eq!(slots.len(), 1, "got {slots:?}");
        assert_eq!(slots[0].entity, "董卓");
        assert_eq!(slots[0].value, "deceased");
    }

    /// Objective: Verify dialogue events never flip state (speech is not
    /// status change) and that a non-kill action (救) flips nothing.
    /// Invariants: both return empty.
    #[test]
    fn dialogue_and_non_kill_actions_yield_no_slots() {
        let src = "刘备曰，赵云救阿斗。";
        let mut dialogue = action_event("刘备 曰", "刘备", None, "刘备曰，");
        dialogue.event_type = "dialogue".into();
        assert!(
            extract_state_slots(&dialogue, src).is_empty(),
            "dialogue must not flip status"
        );
        assert!(
            extract_state_slots(
                &action_event("赵云 救 阿斗", "赵云", Some("阿斗"), src),
                src
            )
            .is_empty(),
            "rescue is not a death"
        );
    }

    /// Objective: Verify a kill verb with NO object participant yields no
    /// slot (we must not guess the victim) — real bug guard against
    /// attributing the killer's own death.
    /// Invariants: empty.
    #[test]
    fn kill_without_object_yields_no_slot() {
        let src = "吕布杀。";
        assert!(
            extract_state_slots(&action_event("吕布 杀", "吕布", None, src), src).is_empty(),
            "no object → no victim to mark"
        );
    }

    /// Objective: Verify the source-context guard rejects the bare-char
    /// substring defect class (the same one ingest DEATH_KW fixed): 死守 /
    /// 殊死 / 杀出 must NOT flip status even though AhoCorasick matched the
    /// single-char verb inside the longer word.
    /// Invariants: all three return empty; 死了-style terminal readings still fire.
    #[test]
    fn embedded_verb_substrings_do_not_flip_status() {
        // 死 inside 死守: after-char 守 is in DEATH_AFTER → no slot.
        let defend = "刘备死守樊城。";
        assert!(
            extract_state_slots(&action_event("刘备 死", "刘备", None, defend), defend).is_empty(),
            "死守 must not mark the defender deceased"
        );

        // 殊死 (before-char 殊 in DEATH_BEFORE): no slot.
        let desperate = "张飞殊死搏斗。";
        assert!(
            extract_state_slots(&action_event("张飞 死", "张飞", None, desperate), desperate)
                .is_empty(),
            "殊死 must not mark the fighter deceased"
        );

        // 杀 inside 杀出重围: after-char 出 is in KILL_AFTER → no slot even
        // with an object participant present in the title.
        let breakout = "吕布杀出重围，遇董卓。";
        assert!(
            extract_state_slots(
                &action_event("吕布 杀 董卓", "吕布", Some("董卓"), breakout),
                breakout
            )
            .is_empty(),
            "杀出 must not mark the object deceased"
        );

        // Terminal readings still fire: 死了 (after=了 NOT in DEATH_AFTER).
        let died = "董卓死了。";
        assert_eq!(
            extract_state_slots(&action_event("董卓 死", "董卓", None, died), died).len(),
            1,
            "死了 is a terminal death and must still flip status"
        );
    }

    /// Objective: Verify the guard degrades to title-only checking when the
    /// source is unavailable (`""`) — callers passing no text must not panic.
    /// Invariants: legacy title-only behavior still yields the kill slot.
    #[test]
    fn empty_source_disables_guard_without_panicking() {
        let slots = extract_state_slots(
            &action_event("吕布 杀 董卓", "吕布", Some("董卓"), "吕布杀董卓。"),
            "",
        );
        assert_eq!(slots.len(), 1, "empty source → guard off, slot still found");
        assert_eq!(slots[0].entity, "董卓");
    }

    /// Objective: Verify multi-word English names ("Mr. Smith") still locate
    /// the verb — positional `[_, verb, _]` parsing read a name fragment as
    /// the verb and silently returned nothing for 4+ token titles.
    /// Invariants: kill marks the OBJECT; the source guard accepts "killed"
    /// as a terminal word-form of the matched "kill" pattern.
    #[test]
    fn english_multiword_name_yields_kill_slot() {
        let src = "Mr. Smith killed Lord Blackwood in the duel.";
        let slots = extract_state_slots(
            &action_event(
                "Mr. Smith kill Lord Blackwood",
                "Mr. Smith",
                Some("Lord Blackwood"),
                src,
            ),
            src,
        );
        assert_eq!(slots.len(), 1, "kill must yield one slot, got {slots:?}");
        assert_eq!(
            slots[0].entity, "Lord Blackwood",
            "victim is the object, not the multi-word subject"
        );
        assert_eq!(slots[0].slot, "status");
        assert_eq!(slots[0].value, "deceased");
        assert_eq!(slots[0].confidence, 0.75);
    }

    /// Objective: Verify an English intransitive death marks the SUBJECT and
    /// that progressive/embedded ASCII matches do NOT flip status — the
    /// AhoCorasick pattern "kill" matches inside "killing"/"skills", and
    /// "die" inside "dies"/"diet" require whole-word acceptance.
    /// Invariants: "died" fires for the subject; "killing" and "skills"
    /// yield no slot even with both participants present.
    #[test]
    fn english_death_fires_and_progressive_does_not() {
        let died_src = "Lord Blackwood died at dawn.";
        let died = extract_state_slots(
            &action_event("Lord Blackwood die", "Lord Blackwood", None, died_src),
            died_src,
        );
        assert_eq!(died.len(), 1, "died must flip the subject, got {died:?}");
        assert_eq!(died[0].entity, "Lord Blackwood");
        assert_eq!(died[0].confidence, 0.7);

        // "killing" (progressive): AC matches the "kill" pattern inside the
        // word — whole-word check must reject it as non-terminal.
        let killing_src = "Mr. Smith was killing Lord Blackwood slowly.";
        let killing = extract_state_slots(
            &action_event(
                "Mr. Smith kill Lord Blackwood",
                "Mr. Smith",
                Some("Lord Blackwood"),
                killing_src,
            ),
            killing_src,
        );
        assert!(
            killing.is_empty(),
            "progressive `killing` must not flip status, got {killing:?}"
        );

        // "skills": "kill" matched mid-word with an object in the title —
        // the embedded-verb guard must still reject.
        let skills_src = "Mr. Smith's skills terrified Lord Blackwood.";
        let skills = extract_state_slots(
            &action_event(
                "Mr. Smith kill Lord Blackwood",
                "Mr. Smith",
                Some("Lord Blackwood"),
                skills_src,
            ),
            skills_src,
        );
        assert!(
            skills.is_empty(),
            "`kill` inside `skills` must not flip status, got {skills:?}"
        );
    }
}
