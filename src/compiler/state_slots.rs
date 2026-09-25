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
/// Single characters here are safe because Pass 2 matches them as *verbs*
/// (AhoCorasick over the lexicon verb tables), not as substrings of prose —
/// a false hit would require the lexicon itself to list a non-kill verb.
const KILL_VERBS: &[&str] = &["杀", "斩", "弑", "诛", "鸩", "戮", "枭"];

/// Intransitive death verbs: the SUBJECT of the event dies.
const DEATH_VERBS: &[&str] = &["死", "亡", "薨", "殁", "卒"];

/// Extract every state-slot flip implied by one Pass 2 event.
///
/// Returns an empty vec for dialogue events and for actions that change no
/// tracked slot — callers must not assume one event yields one slot.
pub fn extract_state_slots(event: &Event) -> Vec<StateSlot> {
    if event.event_type != "action" {
        return Vec::new();
    }
    // Title format from `extract::compile` is "subj verb" or "subj verb obj".
    let tokens: Vec<&str> = event.title.split_whitespace().collect();
    let verb = match tokens.as_slice() {
        [_, verb] => *verb,
        [_, verb, _] => *verb,
        _ => return Vec::new(),
    };

    let (victim_role, confidence) = if KILL_VERBS.contains(&verb) {
        ("object", 0.75)
    } else if DEATH_VERBS.contains(&verb) {
        ("subject", 0.7)
    } else {
        return Vec::new();
    };

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

    fn action_event(title: &str, subj: &str, obj: Option<&str>) -> Event {
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
            start_offset: Some(100),
            end_offset: Some(112),
        }
    }

    /// Objective: Verify a transitive kill marks the OBJECT deceased and
    /// carries the event's chapter + span.
    /// Invariants: one slot; entity == object; status=deceased; offsets match.
    #[test]
    fn kill_verb_marks_object_deceased() {
        let slots = extract_state_slots(&action_event("吕布 杀 董卓", "吕布", Some("董卓")));
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
        assert_eq!(s.start_offset, Some(100));
        assert_eq!(s.end_offset, Some(112));
    }

    /// Objective: Verify an intransitive death marks the SUBJECT deceased.
    /// Invariants: one slot; entity == subject.
    #[test]
    fn death_verb_marks_subject_deceased() {
        let slots = extract_state_slots(&action_event("董卓 死", "董卓", None));
        assert_eq!(slots.len(), 1, "got {slots:?}");
        assert_eq!(slots[0].entity, "董卓");
        assert_eq!(slots[0].value, "deceased");
    }

    /// Objective: Verify dialogue events never flip state (speech is not
    /// status change) and that a non-kill action (救) flips nothing.
    /// Invariants: both return empty.
    #[test]
    fn dialogue_and_non_kill_actions_yield_no_slots() {
        let mut dialogue = action_event("刘备 曰", "刘备", None);
        dialogue.event_type = "dialogue".into();
        assert!(
            extract_state_slots(&dialogue).is_empty(),
            "dialogue must not flip status"
        );
        assert!(
            extract_state_slots(&action_event("赵云 救 阿斗", "赵云", Some("阿斗"))).is_empty(),
            "rescue is not a death"
        );
    }

    /// Objective: Verify a kill verb with NO object participant yields no
    /// slot (we must not guess the victim) — real bug guard against
    /// attributing the killer's own death.
    /// Invariants: empty.
    #[test]
    fn kill_without_object_yields_no_slot() {
        assert!(
            extract_state_slots(&action_event("吕布 杀", "吕布", None)).is_empty(),
            "no object → no victim to mark"
        );
    }
}
