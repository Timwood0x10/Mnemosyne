//! Timeline Builder — Post Pass 2.
//!
//! Detects when relationships change over time by analyzing events in
//! chronological order. Also tracks personality changes and character arcs.
//!
//! ```text
//! Ch.3  吕布serves丁原
//! Ch.9  吕布kills丁原  →  serves relation ENDS at ch.9
//! Ch.14 吕布serves董卓 →  new relation starts at ch.14
//!
//! Personality arc:
//! Ch.3  林黛玉 → 小心谨慎
//! Ch.27 林黛玉 → 多愁善感
//! Ch.97 林黛玉 → 绝望离世
//! ```

use std::collections::HashMap;

use crate::compiler::{Event, Relation};

/// A personality marker at a point in time.
#[derive(Debug, Clone)]
pub struct PersonalityMarker {
    pub entity: String,
    pub chapter: i32,
    pub trait_name: String,     // "小心谨慎" / "刚烈" / "多愁善感"
    pub context: String,        // "初入贾府" / "葬花" / "焚稿"
    pub confidence: f64,
}

/// A detected character arc — change in personality over time.
#[derive(Debug, Clone)]
pub struct CharacterArc {
    pub entity: String,
    pub arc: Vec<String>,       // ["小心谨慎", "多愁善感", "绝望离世"]
    pub arc_type: String,       // "growth" / "decline" / "transformation"
}

/// Verbs that indicate a hostile relationship change.
const HOSTILE_VERBS: &[&str] = &["杀", "斩", "攻", "围", "擒", "缚", "绑", "骂", "怒", "打", "刺", "射"];

/// Words that indicate personality/character description in Chinese text.
const PERSONALITY_KEYWORDS: &[(&str, &str)] = &[
    ("性刚烈", "刚烈"),
    ("性温柔", "温柔"),
    ("性懦弱", "懦弱"),
    ("性聪慧", "聪慧"),
    ("性多疑", "多疑"),
    ("性宽厚", "宽厚"),
    ("性急", "性急"),
    ("性躁", "急躁"),
    ("性残忍", "残忍"),
    ("心善", "善良"),
    ("心狠", "狠毒"),
    ("心窄", "心胸狭窄"),
    ("性格", ""),   // catch-all followed by extraction
    ("为人", ""),   // catch-all
];

/// Extract personality markers from events AND from raw text.
///
/// Scans event descriptions for personality keywords.
/// Also scans the original text (`corpus_text`) — this catches descriptions
/// that aren't captured as events (e.g. narrative introductions).
pub fn extract_personality_markers(
    events: &[Event],
    corpus_text: Option<&str>,
    known_entities: &[String],
) -> Vec<PersonalityMarker> {
    let mut markers = Vec::new();

    // Pass 1: scan event titles
    for event in events {
        let ts = event.timestamp.unwrap_or(0);
        for participant in &event.participants {
            for &(keyword, trait_name) in PERSONALITY_KEYWORDS {
                if event.title.contains(keyword) {
                    check_and_push(&event.title, &participant.entity_name,
                        keyword, trait_name, ts, &event.title, &mut markers);
                }
            }
        }
    }

    // Pass 2: scan the raw text (catches narrative descriptions not in events)
    if let Some(text) = corpus_text {
        for line in text.lines() {
            let line = line.trim();
            if line.len() < 10 { continue; }
            for &(keyword, trait_name) in PERSONALITY_KEYWORDS {
                if !line.contains(keyword) { continue; }
                // Find which known entity is near this keyword
                for entity in known_entities {
                    if line.contains(entity.as_str()) {
                        check_and_push(line, entity, keyword, trait_name,
                            0, line.chars().take(120).collect::<String>().as_str(), &mut markers);
                        break;
                    }
                }
            }
        }
    }

    markers
}

/// Helper: check if entity and keyword are close, then push marker.
fn check_and_push(
    text: &str, entity: &str, keyword: &str, trait_name: &str,
    chapter: i32, context: &str, markers: &mut Vec<PersonalityMarker>,
) {
    if let (Some(kpos), Some(npos)) = (text.find(keyword), text.find(entity)) {
        let dist = if npos > kpos { npos - kpos } else { kpos - npos };
        if dist < 25 {
            let trait_val = if trait_name.is_empty() {
                let after = &text[kpos + keyword.len()..];
                after.chars().take_while(|c| *c != '，' && *c != '。' && *c != '；').collect::<String>()
            } else {
                trait_name.to_string()
            };
            if !trait_val.is_empty() {
                markers.push(PersonalityMarker {
                    entity: entity.to_string(),
                    chapter,
                    trait_name: trait_val,
                    context: context.to_string(),
                    confidence: 0.7,
                });
            }
        }
    }
}

/// Build character arcs from personality markers.
///
/// Groups markers by entity, sorts by chapter, and detects the arc type:
/// - "decline": 从正面到负面 (善良→狠毒)
/// - "growth": 从负面到正面 (懦弱→勇敢)
/// - "transformation": 单一变化
/// - "stable": 只有一种描摹
pub fn build_character_arcs(markers: Vec<PersonalityMarker>) -> Vec<CharacterArc> {
    let mut by_entity: HashMap<String, Vec<PersonalityMarker>> = HashMap::new();
    for m in markers {
        by_entity.entry(m.entity.clone()).or_default().push(m);
    }

    let mut arcs = Vec::new();
    for (entity, mut markers) in by_entity {
        markers.sort_by_key(|m| m.chapter);
        let traits: Vec<String> = markers.iter().map(|m| m.trait_name.clone()).collect();
        let arc_type = if traits.len() <= 1 {
            "stable"
        } else {
            // Simple heuristic: check if first and last differ significantly
            let first = &traits[0];
            let last = &traits[traits.len() - 1];
            if first != last {
                "transformation"
            } else {
                "stable"
            }
        };
        arcs.push(CharacterArc {
            entity,
            arc: traits,
            arc_type: arc_type.into(),
        });
    }
    arcs
}

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
            effects: vec![],
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
