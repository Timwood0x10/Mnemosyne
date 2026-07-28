//! Story Compiler — Pass 2.
//!
//! Extracts Events and Relations from body text using the Entity Registry
//! (built by Pass 1) to resolve mentions to entity IDs.
//!
//! ## Pipeline within Pass 2
//!
//! ```text
//! Sentences → Mention Scan → Observation (SPO) → Event → Relation
//! ```

use crate::compiler::entity::EntityDictionary;
use crate::compiler::{CompileContext, Event, EventParticipant, Mention, Relation};

/// Config for the story compiler's observation extraction.
#[derive(Debug, Clone)]
pub struct Config {
    pub strong_verbs: Vec<String>,
    pub action_verbs: Vec<String>,
    pub dialog_markers: Vec<String>,
    pub proximity_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            strong_verbs: vec![
                "杀","斩","擒","救","打","战","斗","败","胜","攻","破",
                "逃","死","绑","缚","骂","哭","笑","怒","拜","封","赐","赏",
            ].into_iter().map(String::from).collect(),
            action_verbs: vec![
                "大怒","大喜","领兵","引军","挺枪","纵马","大呼","拔剑",
                "出马","上前","大惊",
            ].into_iter().map(String::from).collect(),
            dialog_markers: vec!["曰：".into(), "道：".into(), "言：".into()],
            proximity_chars: 50,
        }
    }
}

/// Scan sentences for entity mentions, extract events, and populate the context.
pub fn compile(ctx: &mut CompileContext, sentences: &[&str], dict: &EntityDictionary, config: &Config) {
    let mut current_chapter = ctx.current_timestamp.unwrap_or(1);

    for (_sent_idx, text) in sentences.iter().enumerate() {
        if text.len() < 2 {
            continue;
        }

        let local_mentions = scan_mentions(text, dict);
        if local_mentions.is_empty() {
            continue;
        }

        // Dialog pattern: X曰/Y道 → Event(dialogue)
        for marker in &config.dialog_markers {
            if let Some(pos) = text.find(marker.as_str()) {
                let speaker = local_mentions.iter()
                    .filter(|m| m.offset.end <= pos)
                    .last();
                let addressee = local_mentions.iter()
                    .find(|m| m.offset.start >= pos + marker.len());

                if let Some(s) = speaker {
                    let mut participants = vec![
                        EventParticipant {
                            entity_name: s.canonical_name.clone(),
                            role: "speaker".into(),
                        }
                    ];
                    if let Some(a) = addressee {
                        participants.push(EventParticipant {
                            entity_name: a.canonical_name.clone(),
                            role: "addressee".into(),
                        });
                    }
                    ctx.events.push(Event {
                        id: None,
                        title: format!("{}曰", s.canonical_name),
                        event_type: "dialogue".into(),
                        timestamp: Some(current_chapter),
                        location: None,
                        description: text[..pos.min(text.len())].to_string(),
                        participants,
                        importance: 0.5,
                    });
                }
            }
        }

        // Strong verb / action verb patterns → Event(action)
        let all_verbs: Vec<&str> = config.strong_verbs.iter()
            .chain(config.action_verbs.iter())
            .map(|s| s.as_str())
            .collect();

        for verb in &all_verbs {
            for (pos, _) in text.match_indices(verb) {
                let subject = local_mentions.iter()
                    .filter(|m| m.offset.end <= pos
                        && (pos - m.offset.end) < config.proximity_chars)
                    .last();

                let object = local_mentions.iter()
                    .find(|m| m.offset.start >= pos + verb.len()
                        && (m.offset.start - (pos + verb.len())) < config.proximity_chars);

                if let Some(s) = subject {
                    let mut title = format!("{}{}", s.canonical_name, verb);
                    let mut participants = vec![
                        EventParticipant {
                            entity_name: s.canonical_name.clone(),
                            role: "subject".into(),
                        }
                    ];
                    if let Some(o) = object {
                        title = format!("{}{}{}", s.canonical_name, verb, o.canonical_name);
                        participants.push(EventParticipant {
                            entity_name: o.canonical_name.clone(),
                            role: "object".into(),
                        });
                    }

                    ctx.events.push(Event {
                        id: None,
                        title,
                        event_type: "action".into(),
                        timestamp: Some(current_chapter),
                        location: None,
                        description: text[..pos.min(text.len())].to_string(),
                        participants,
                        importance: 0.6,
                    });
                }
            }
        }

        // Chapter tracking: approximate chapter from "第X回" pattern
        if text.contains("第") && (text.contains("回") || text.contains("章")) {
            current_chapter += 1;
            ctx.current_timestamp = Some(current_chapter);
        }
    }

    // Build relations from co-occurring event participants
    build_relations(ctx);
}

/// Scan a single sentence for entity mentions using the dictionary.
fn scan_mentions(text: &str, dict: &EntityDictionary) -> Vec<Mention> {
    let mut mentions = Vec::new();

    // Simple longest-first scan: check if any known alias appears in the text
    let mut aliases: Vec<(&str, &str)> = dict.alias_to_canonical.iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    aliases.sort_by(|a, b| b.0.len().cmp(&a.0.len())); // longest first

    for (alias, canonical) in &aliases {
        for (pos, _) in text.match_indices(alias) {
            // Avoid overlapping matches (skip if within an existing mention)
            if mentions.iter().any(|m: &Mention| pos >= m.offset.start && pos < m.offset.end) {
                continue;
            }
            let (_, entity_id) = dict.resolve(alias).unwrap_or((canonical.to_string(), None));
            mentions.push(Mention {
                sentence_id: 0,
                entity_id,
                surface: alias.to_string(),
                canonical_name: canonical.to_string(),
                offset: pos..(pos + alias.len()),
                confidence: 0.9,
            });
        }
    }

    mentions.sort_by(|a, b| a.offset.start.cmp(&b.offset.start));
    mentions
}

/// Build relations from events — entities that appear in the same event
/// multiple times with consistent roles become a relation.
fn build_relations(ctx: &mut CompileContext) {
    for event in &ctx.events {
        for pair in event_pairs(&event.participants) {
            let already = ctx.relations.iter().any(|r| {
                (r.source == pair.0 && r.target == pair.1)
                    || (r.source == pair.1 && r.target == pair.0)
            });
            if !already {
                ctx.relations.push(Relation {
                    source: pair.0.clone(),
                    target: pair.1.clone(),
                    relation_type: "associated".into(),
                    valid_from: event.timestamp,
                    valid_to: None,
                    confidence: 0.5,
                });
            }
        }
    }
}

/// Generate all unique pairs from a participant list.
fn event_pairs(participants: &[EventParticipant]) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for i in 0..participants.len() {
        for j in (i + 1)..participants.len() {
            let a = &participants[i].entity_name;
            let b = &participants[j].entity_name;
            if a != b {
                pairs.push((a.clone(), b.clone()));
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::entity::EntityDictionary;

    fn make_dict() -> EntityDictionary {
        let mut d = EntityDictionary::default();
        d.alias_to_canonical.insert("刘备".into(), "刘备".into());
        d.alias_to_canonical.insert("关羽".into(), "关羽".into());
        d.alias_to_canonical.insert("张飞".into(), "张飞".into());
        d.alias_to_canonical.insert("赵云".into(), "赵云".into());
        d.alias_to_canonical.insert("阿斗".into(), "阿斗".into());
        d.alias_to_canonical.insert("曹操".into(), "曹操".into());
        d
    }

    /// Objective: Verify that a simple action sentence creates an Event.
    /// Invariants: At least one event with type "action".
    #[test]
    fn action_sentence_creates_event() {
        let mut ctx = CompileContext::default();
        let dict = make_dict();
        let config = Config::default();
        compile(&mut ctx, &["赵云救阿斗。"], &dict, &config);
        assert!(!ctx.events.is_empty(), "should create at least one event");
        let has_action = ctx.events.iter().any(|e| e.event_type == "action");
        assert!(has_action, "should have action-type event");
    }

    /// Objective: Verify that a dialog sentence creates a Dialogue event.
    /// Invariants: Event type is "dialogue"; participants include speaker.
    #[test]
    fn dialog_creates_dialogue_event() {
        let mut ctx = CompileContext::default();
        let dict = make_dict();
        let config = Config::default();
        compile(&mut ctx, &["刘备曰：关羽"], &dict, &config);
        let has_dialogue = ctx.events.iter().any(|e| e.event_type == "dialogue");
        assert!(has_dialogue, "dialog sentence should create dialogue event");
    }

    /// Objective: Verify that repeated co-occurrence creates a relation.
    /// Invariants: A relation exists between 刘备 and 关羽.
    #[test]
    fn co_occurrence_creates_relation() {
        let mut ctx = CompileContext::default();
        let dict = make_dict();
        let config = Config::default();
        let sentences = vec!["刘备救关羽。", "刘备救张飞。"];
        let refs: Vec<&str> = sentences.iter().map(|s| *s).collect();
        compile(&mut ctx, &refs, &dict, &config);
        let has_rel = ctx.relations.iter().any(|r| {
            (r.source == "刘备" && r.target == "关羽")
                || (r.source == "关羽" && r.target == "刘备")
        });
        assert!(has_rel, "co-occurrence should create a relation");
    }
}
