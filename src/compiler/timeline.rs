//! Timeline Builder — Post Pass 2.
//!
//! Detects when relationships change over time by analyzing events in
//! chronological order. For example:
//!
//! ```text
//! Ch.3  吕布serves丁原
//! Ch.9  吕布kills丁原  →  serves relation ENDS at ch.9
//! Ch.14 吕布serves董卓 →  new relation starts at ch.14
//! Ch.19 吕布kills董卓  →  serves relation ENDS at ch.19
//! ```
//!
//! The tracker outputs updated [`Relation`]s with `valid_from`/`valid_to` set.

use std::collections::HashMap;

use crate::compiler::{Event, Relation};

/// Verbs that indicate a hostile relationship change.
const HOSTILE_VERBS: &[&str] = &["杀", "斩", "攻", "围", "擒", "缚", "绑", "骂", "怒", "打", "刺", "射"];

/// Verbs that indicate a positive relationship change.
const FRIENDLY_VERBS: &[&str] = &["救", "拜", "封", "赐", "赏", "嫁", "娶"];

/// Build a temporally-aware relation set from a time-ordered event list.
///
/// # Algorithm
///
/// 1. Group events by timestamp (chapter number).
/// 2. For each event, determine the relationship change it represents.
/// 3. For each (entity_a, entity_b) pair, track the active relation type
///    and when it started.
/// 4. When a conflicting event occurs (e.g., 杀 after 服), end the old
///    relation and start a new one.
/// 5. Output relations with `valid_from` and `valid_to` populated.
pub fn build_timeline(events: &[Event], existing: &[Relation]) -> Vec<Relation> {
    // Sort events by timestamp
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| e.timestamp.unwrap_or(0));

    // Track current relation state per entity pair
    // Key: (source, target) → current relation type
    let mut current: HashMap<(String, String), (String, Option<i32>)> = HashMap::new();
    let mut results: Vec<Relation> = Vec::new();

    // Seed with existing relations (default valid_from=1)
    for r in existing {
        let key = (r.source.clone(), r.target.clone());
        current.entry(key).or_insert_with(|| (r.relation_type.clone(), r.valid_from));
    }

    for event in &sorted {
        let ts = event.timestamp;
        let participants: Vec<&str> = event.participants.iter().map(|p| p.entity_name.as_str()).collect();

        // Skip events without at least 2 participants
        if participants.len() < 2 {
            continue;
        }

        // Analyze each pair in this event
        for i in 0..participants.len() {
            for j in (i + 1)..participants.len() {
                let a = participants[i];
                let b = participants[j];
                if a == b { continue; }

                // Determine relation type from event type and predicate
                let rel_type = infer_relation_type(event, a, b);

                let key_a_b = (a.to_string(), b.to_string());
                let key_b_a = (b.to_string(), a.to_string());

                // Check if this changes an existing relation
                for key in &[&key_a_b, &key_b_a] {
                    if let Some((old_type, started)) = current.get(key) {
                        if *old_type != rel_type {
                            // Relation changed — close the old one
                            results.push(Relation {
                                source: key.0.clone(),
                                target: key.1.clone(),
                                relation_type: old_type.clone(),
                                valid_from: *started,
                                valid_to: ts,
                                confidence: 0.8,
                            });
                        }
                    }
                }

                // Update current state
                current.insert(key_a_b.clone(), (rel_type.clone(), ts));
                current.insert(key_b_a.clone(), (rel_type.clone(), ts));
            }
        }
    }

    // Flush remaining active relations
    for ((source, target), (rel_type, started)) in current.drain() {
        results.push(Relation {
            source,
            target,
            relation_type: rel_type,
            valid_from: started,
            valid_to: None, // ongoing
            confidence: 0.8,
        });
    }

    results
}

/// Infer the relation type between two participants of an event.
fn infer_relation_type(event: &Event, a: &str, b: &str) -> String {
    // Check for hostile verbs in the event's predicate (title)
    let pred_lower = event.title.to_lowercase();

    // If this is a hostile event and A kills/harms B, mark as "enemy"
    if let Some(subject) = event.participants.iter().find(|p| p.role == "subject") {
        if subject.entity_name == a || subject.entity_name == b {
            for verb in HOSTILE_VERBS {
                if event.title.contains(verb) {
                    return "enemy".into();
                }
            }
            for verb in FRIENDLY_VERBS {
                if event.title.contains(verb) {
                    return "ally".into();
                }
            }
        }
    }

    // Default: participants in the same event are "associated"
    "associated".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::EventParticipant;

    fn mk_event(ts: i32, title: &str, subj: &str, obj: &str) -> Event {
        Event {
            id: None,
            title: title.into(),
            event_type: "action".into(),
            timestamp: Some(ts),
            location: None,
            description: String::new(),
            participants: vec![
                EventParticipant { entity_name: subj.into(), role: "subject".into() },
                EventParticipant { entity_name: obj.into(), role: "object".into() },
            ],
            importance: 0.5,
        }
    }

    /// Objective: Verify that 吕布杀丁原 ends the associated relation
    /// and creates an enemy relation.
    /// Invariants: Associated relation has valid_to=9; enemy relation exists.
    #[test]
    fn kill_ends_previous_relation() {
        let events = vec![
            mk_event(3, "吕布服丁原", "吕布", "丁原"),
            mk_event(9, "吕布杀丁原", "吕布", "丁原"),
        ];
        let rels = build_timeline(&events, &[]);
        let associated: Vec<&Relation> = rels.iter().filter(|r| r.relation_type == "associated").collect();
        let enemy: Vec<&Relation> = rels.iter().filter(|r| r.relation_type == "enemy").collect();
        assert!(!associated.is_empty(), "should have associated relation before ch.9");
        assert!(!enemy.is_empty(), "should have enemy relation after ch.9");
        // The associated relation should be closed at ch.9
        assert_eq!(associated[0].valid_to, Some(9), "associated should end at ch.9");
    }

    /// Objective: Verify that 救 creates an ally relation.
    /// Invariants: Relation type is "ally".
    #[test]
    fn rescue_creates_ally() {
        let events = vec![
            mk_event(5, "赵云救阿斗", "赵云", "阿斗"),
        ];
        let rels = build_timeline(&events, &[]);
        let ally = rels.iter().find(|r| r.relation_type == "ally");
        assert!(ally.is_some(), "rescue should create ally relation");
        assert_eq!(ally.unwrap().valid_from, Some(5));
        assert_eq!(ally.unwrap().valid_to, None);
    }
}
