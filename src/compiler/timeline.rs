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
    pub trait_name: String, // "小心谨慎" / "刚烈" / "多愁善感"
    pub context: String,    // "初入贾府" / "葬花" / "焚稿"
    pub confidence: f64,
}

/// A detected character arc — change in personality over time.
#[derive(Debug, Clone)]
pub struct CharacterArc {
    pub entity: String,
    pub arc: Vec<String>, // ["小心谨慎", "多愁善感", "绝望离世"]
    pub arc_type: String, // "growth" / "decline" / "transformation"
}

/// Configurable verbs for timeline building.
pub struct TimelineConfig {
    pub hostile_verbs: Vec<String>,
    pub friendly_verbs: Vec<String>,
}

impl Default for TimelineConfig {
    fn default() -> Self {
        TimelineConfig {
            // Verbs come from the lexicon registry (single source of truth).
            hostile_verbs: crate::lexicon::global()
                .zh_hostile()
                .iter()
                .cloned()
                .collect(),
            friendly_verbs: crate::lexicon::global()
                .zh_friendly()
                .iter()
                .cloned()
                .collect(),
        }
    }
}

impl TimelineConfig {
    /// Build a timeline config from a language provider's verb definitions.
    pub fn from_language(lang: &dyn crate::language::LanguageProvider) -> Self {
        TimelineConfig {
            hostile_verbs: lang.hostile_verbs(),
            friendly_verbs: lang.friendly_verbs(),
        }
    }
}

/// Extract personality markers from events AND from raw text.
///
/// Scans event descriptions for personality keywords.
/// Also scans the original text (`corpus_text`) — this catches descriptions
/// that aren't captured as events (e.g. narrative introductions).
///
/// `patterns` is the list of trigger patterns to scan for (性, 为人, 平生...).
/// These come from the JSON config profile, not from hardcoded code.
pub fn extract_personality_markers(
    events: &[Event],
    corpus_text: Option<&str>,
    known_entities: &[String],
    patterns: &[String],
) -> Vec<PersonalityMarker> {
    let mut markers = Vec::new();

    // Pass 1: scan event titles
    for event in events {
        let ts = event.timestamp.unwrap_or(0);
        for participant in &event.participants {
            for kw in patterns {
                if event.title.contains(kw.as_str()) {
                    check_and_push(
                        &event.title,
                        &participant.entity_name,
                        kw.as_str(),
                        ts,
                        &event.title,
                        &mut markers,
                    );
                }
            }
        }
    }

    // Pass 2: scan the raw text (catches narrative descriptions not in events)
    if let Some(text) = corpus_text {
        for line in text.lines() {
            let line = line.trim();
            if line.len() < 10 {
                continue;
            }
            for kw in patterns {
                if !line.contains(kw.as_str()) {
                    continue;
                }
                // Find which known entity is near this keyword
                for entity in known_entities {
                    if line.contains(entity.as_str()) {
                        check_and_push(
                            line,
                            entity,
                            kw.as_str(),
                            0,
                            line.chars().take(120).collect::<String>().as_str(),
                            &mut markers,
                        );
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
    text: &str,
    entity: &str,
    keyword: &str,
    chapter: i32,
    context: &str,
    markers: &mut Vec<PersonalityMarker>,
) {
    if let (Some(kpos), Some(npos)) = (text.find(keyword), text.find(entity)) {
        let dist = npos.abs_diff(kpos);
        if dist < 25 {
            let trait_val = {
                let after = &text[kpos + keyword.len()..];
                after
                    .chars()
                    .take_while(|c| *c != '，' && *c != '。' && *c != '；')
                    .collect::<String>()
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
/// - "decline": 正面→负面轨迹 (仁义→狡诈)
/// - "growth": 负面→正面轨迹 (浅薄→深沉)
/// - "transformation": 方向不明的改变
/// - "stable": 一成不变
pub fn build_character_arcs(markers: Vec<PersonalityMarker>) -> Vec<CharacterArc> {
    // Positive vs negative trait classification
    let positive: &[&str] = &[
        "仁", "义", "善", "宽", "厚", "谦", "智勇", "英雄", "忠", "慈",
    ];
    let negative: &[&str] = &[
        "奸",
        "狡",
        "诈",
        "狠",
        "毒",
        "多疑",
        "善妒",
        "刚愎",
        "傲慢",
        "躁",
        "急",
        "残",
        "有勇无谋",
    ];

    fn classify_trait(t: &str, pos: &[&str], neg: &[&str]) -> i32 {
        if pos.iter().any(|p| t.contains(p)) {
            1
        } else if neg.iter().any(|n| t.contains(n)) {
            -1
        } else {
            0
        }
    }

    let mut by_entity: HashMap<String, Vec<PersonalityMarker>> = HashMap::new();
    for m in markers {
        by_entity.entry(m.entity.clone()).or_default().push(m);
    }

    let mut arcs = Vec::new();
    for (entity, mut entity_markers) in by_entity {
        entity_markers.sort_by_key(|m| m.chapter);
        let traits: Vec<String> = entity_markers
            .iter()
            .map(|m| m.trait_name.clone())
            .collect();

        let arc_type = if traits.len() <= 1 {
            "stable"
        } else {
            let first = &traits[0];
            let last = &traits[traits.len() - 1];
            if first == last {
                "stable"
            } else {
                let fv = classify_trait(first, positive, negative);
                let lv = classify_trait(last, positive, negative);
                if fv > 0 && lv < 0 {
                    "decline" // 正面→负面
                } else if fv < 0 && lv > 0 {
                    "growth" // 负面→正面
                } else {
                    "transformation"
                }
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

/// Build a temporally-aware relation set from a time-ordered event list.
///
/// Uses the provided `config` (hostile/friendly verb lists from JSON profile)
/// to determine relationship types.
///
/// # Algorithm
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
pub fn build_timeline(
    events: &[Event],
    existing: &[Relation],
    config: &TimelineConfig,
) -> Vec<Relation> {
    // Sort events by timestamp
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| e.timestamp.unwrap_or(0));

    // Track current relation state per entity pair
    // Key: (source, target) → current relation type
    let mut current: HashMap<(String, String), (String, Option<i32>)> = HashMap::new();
    let mut results: Vec<Relation> = Vec::new();

    // Seed with existing relations (default valid_from=1).
    // Seed BOTH directions so the event loop's bidirectional lookup finds
    // them regardless of participant order.
    for r in existing {
        let key_fwd = (r.source.clone(), r.target.clone());
        let key_rev = (r.target.clone(), r.source.clone());
        current
            .entry(key_fwd)
            .or_insert_with(|| (r.relation_type.clone(), r.valid_from));
        current
            .entry(key_rev)
            .or_insert_with(|| (r.relation_type.clone(), r.valid_from));
    }

    for event in &sorted {
        let ts = event.timestamp;
        let participants: Vec<&str> = event
            .participants
            .iter()
            .map(|p| p.entity_name.as_str())
            .collect();

        // Skip events without at least 2 participants
        if participants.len() < 2 {
            continue;
        }

        // Analyze each pair in this event
        for i in 0..participants.len() {
            for j in (i + 1)..participants.len() {
                let a = participants[i];
                let b = participants[j];
                if a == b {
                    continue;
                }

                // Determine relation type from event type and predicate
                let rel_type = infer_relation_type(event, a, b, config);

                let key_a_b = (a.to_string(), b.to_string());
                let key_b_a = (b.to_string(), a.to_string());

                // Check if this changes an existing relation. Only close +
                // update when the type ACTUALLY changes — the old code
                // unconditionally inserted, clobbering valid_from on every
                // event even when the type was unchanged (NEW-H22).
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
                            // Update to the new type + timestamp
                            current.insert((key.0.clone(), key.1.clone()), (rel_type.clone(), ts));
                        }
                        // If type is unchanged, keep the existing valid_from
                        // (do NOT clobber it with the current timestamp).
                    } else {
                        // New relation — insert with current timestamp
                        current.insert((key.0.clone(), key.1.clone()), (rel_type.clone(), ts));
                    }
                }
            }
        }
    }

    // Flush remaining active relations. Dedup reverse-direction pairs: the
    // event loop stores both (A,B) and (B,A) with the same type, so emitting
    // both would double every ongoing relation (NEW-H23). We canonicalize to
    // the lexicographically-smaller direction and skip the reverse.
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for ((source, target), (rel_type, started)) in current.drain() {
        let canon_key = if source <= target {
            (source.clone(), target.clone())
        } else {
            (target.clone(), source.clone())
        };
        if !seen.insert(canon_key) {
            continue; // reverse direction already emitted
        }
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
/// Uses the hostile/friendly verb lists from the timeline config.
fn infer_relation_type(event: &Event, a: &str, b: &str, config: &TimelineConfig) -> String {
    // If this is a hostile event and A kills/harms B, mark as "enemy"
    if let Some(subject) = event.participants.iter().find(|p| p.role == "subject") {
        if subject.entity_name == a || subject.entity_name == b {
            for verb in &config.hostile_verbs {
                if event.title.contains(verb.as_str()) {
                    return "enemy".into();
                }
            }
            for verb in &config.friendly_verbs {
                if event.title.contains(verb.as_str()) {
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
                EventParticipant {
                    entity_name: subj.into(),
                    role: "subject".into(),
                },
                EventParticipant {
                    entity_name: obj.into(),
                    role: "object".into(),
                },
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
        let rels = build_timeline(&events, &[], &TimelineConfig::default());
        let associated: Vec<&Relation> = rels
            .iter()
            .filter(|r| r.relation_type == "associated")
            .collect();
        let enemy: Vec<&Relation> = rels.iter().filter(|r| r.relation_type == "enemy").collect();
        assert!(
            !associated.is_empty(),
            "should have associated relation before ch.9"
        );
        assert!(!enemy.is_empty(), "should have enemy relation after ch.9");
        // The associated relation should be closed at ch.9
        assert_eq!(
            associated[0].valid_to,
            Some(9),
            "associated should end at ch.9"
        );
    }

    /// Objective: Verify that 救 creates an ally relation.
    /// Invariants: Relation type is "ally".
    #[test]
    fn rescue_creates_ally() {
        let events = vec![mk_event(5, "赵云救阿斗", "赵云", "阿斗")];
        let rels = build_timeline(&events, &[], &TimelineConfig::default());
        let ally = rels.iter().find(|r| r.relation_type == "ally");
        assert!(ally.is_some(), "rescue should create ally relation");
        assert_eq!(ally.unwrap().valid_from, Some(5));
        assert_eq!(ally.unwrap().valid_to, None);
    }

    /// Objective: Verify that an unchanged relation type does NOT clobber its
    /// valid_from timestamp (NEW-H22 regression lock).
    /// Invariants: Two associated events at ts=3 and ts=9 (same type) keep
    /// valid_from=3 on the ongoing relation; no closed duplicate is emitted.
    #[test]
    fn unchanged_type_keeps_valid_from() {
        let events = vec![
            mk_event(3, "吕布服丁原", "吕布", "丁原"),
            mk_event(9, "吕布从丁原", "吕布", "丁原"),
        ];
        let rels = build_timeline(&events, &[], &TimelineConfig::default());
        let ongoing = rels
            .iter()
            .filter(|r| r.relation_type == "associated" && r.valid_to.is_none())
            .collect::<Vec<_>>();
        assert_eq!(
            ongoing.len(),
            1,
            "exactly one ongoing relation expected, got {ongoing:?}"
        );
        assert_eq!(
            ongoing[0].valid_from,
            Some(3),
            "valid_from must stay at first occurrence, got {:?}",
            ongoing[0].valid_from
        );
        // No closed relation for the same pair should exist (type unchanged).
        let closed = rels
            .iter()
            .filter(|r| r.relation_type == "associated" && r.valid_to.is_some())
            .count();
        assert_eq!(closed, 0, "no closed relation when type never changed");
    }

    /// Objective: Verify that reverse-direction duplicates are deduplicated
    /// (NEW-H23 regression lock).
    /// Invariants: A single two-participant event produces ONE relation row,
    /// not both (A,B) and (B,A).
    #[test]
    fn reverse_direction_deduplicated() {
        let events = vec![mk_event(5, "吕布服丁原", "吕布", "丁原")];
        let rels = build_timeline(&events, &[], &TimelineConfig::default());
        assert_eq!(
            rels.len(),
            1,
            "one event with two participants must yield exactly one relation, got {rels:?}"
        );
    }
}
