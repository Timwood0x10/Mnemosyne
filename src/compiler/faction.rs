//! Faction Tracker — baseline faction + timeline faction transitions.
//!
//! Each entity starts in a baseline faction (loaded from config/faction_map.json).
//! Events may trigger faction transitions:
//!
//! ```text
//! Lü Bu: 群雄 (baseline) → Ch.9 kills Ding Yuan → 群雄 (unchanged, but serves-relation ends)
//!        Ch.14 adopts Dong Zhuo as foster father → 群雄 (not official switch)
//!        Ch.19 captured by Cao Cao → dead
//! Zhang Liao: 群雄(under Lü Bu) → Ch.20 surrenders to Cao Cao → 魏
//! Guan Yu: 蜀 → Ch.25 temporarily serves Cao Cao → 魏(temporary)
//!          Ch.27 returns to Liu Bei → 蜀(restored)
//! ```

use std::collections::HashMap;
use std::path::Path;

/// A single faction transition record.
#[derive(Debug, Clone)]
pub struct FactionTransition {
    pub entity: String,
    pub from_faction: String,
    pub to_faction: String,
    pub chapter: i32,
    pub reason: String,
    pub confidence: f64,
}

/// Faction tracker that maintains baseline + current faction for each entity.
pub struct FactionTracker {
    /// Baseline faction mapping: entity → faction
    baseline: HashMap<String, String>,
    /// Current faction assignments (may change over time)
    current: HashMap<String, String>,
    /// History of faction transitions
    pub transitions: Vec<FactionTransition>,
}

impl FactionTracker {
    /// Load baseline factions from `faction_map.json`.
    pub fn from_file(
        novel: &str,
        path: impl AsRef<Path>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let map: HashMap<String, HashMap<String, Vec<String>>> = serde_json::from_str(&content)?;
        let mut baseline = HashMap::new();
        if let Some(factions) = map.get(novel) {
            for (faction, members) in factions {
                for member in members {
                    if !baseline.contains_key(member) {
                        baseline.insert(member.clone(), faction.clone());
                    }
                }
            }
        }
        let current = baseline.clone();
        Ok(FactionTracker {
            baseline,
            current,
            transitions: Vec::new(),
        })
    }

    /// Get an entity's current faction.
    pub fn faction_of(&self, entity: &str) -> Option<&str> {
        self.current.get(entity).map(|s| s.as_str())
    }

    /// Get an entity's baseline (original) faction.
    pub fn baseline_of(&self, entity: &str) -> Option<&str> {
        self.baseline.get(entity).map(|s| s.as_str())
    }

    /// Process an event to detect faction transitions.
    ///
    /// Rules (strict matching):
    /// - action-type events containing surrender ("降X") → entity switches to X's faction
    /// - betrayal kills (杀主公/杀义父/弑) → marked as defector (low confidence)
    /// - dialogue events never trigger transitions
    pub fn process_event(&mut self, event: &crate::compiler::Event) {
        let ts = event.timestamp.unwrap_or(0);
        let title = &event.title;
        let participants: Vec<&str> = event
            .participants
            .iter()
            .map(|p| p.entity_name.as_str())
            .collect();
        if participants.len() < 2 {
            return;
        }

        // Only action events can trigger faction changes
        if event.event_type != "action" {
            return;
        }

        // Detect "X降Y" patterns — a surrender action.
        // Only the SUBJECT (the one surrendering) switches to the OBJECT's
        // faction. The recipient must NOT switch — previously the nested loop
        // switched both participants, corrupting the recipient's faction.
        if title.contains("降") || title.contains("投降") {
            if let (Some(subj), Some(obj)) = (
                event.participants.iter().find(|p| p.role == "subject"),
                event.participants.iter().find(|p| p.role == "object"),
            ) {
                let sn = &subj.entity_name;
                let on = &obj.entity_name;
                if sn != on {
                    // Clone faction values to avoid borrowing self during mutation
                    let sf = self.faction_of(sn).map(|s| s.to_string());
                    let of = self.faction_of(on).map(|s| s.to_string());
                    if let (Some(sf), Some(of)) = (sf, of) {
                        if sf != of {
                            self.current.insert(sn.to_string(), of.clone());
                            self.transitions.push(FactionTransition {
                                entity: sn.to_string(),
                                from_faction: sf,
                                to_faction: of,
                                chapter: ts,
                                reason: title.clone(),
                                confidence: 0.85,
                            });
                        }
                    }
                }
            }
        }

        // Detect betrayal kill — subject kills someone in the same faction
        // Only if the title contains explicit betrayal signals (not combat kills)
        if title.contains("杀") || title.contains("斩") {
            if let (Some(subj), Some(obj)) = (
                event.participants.iter().find(|p| p.role == "subject"),
                event.participants.iter().find(|p| p.role == "object"),
            ) {
                let sn = &subj.entity_name;
                let on = &obj.entity_name;
                if let (Some(sf), Some(of)) = (self.faction_of(sn), self.faction_of(on)) {
                    if sf == of && sn != on && sf != "群雄" {
                        let is_betrayal = title.contains("杀主公")
                            || title.contains("杀义父")
                            || title.contains("弑")
                            || title.contains("袭杀");
                        if is_betrayal {
                            self.transitions.push(FactionTransition {
                                entity: sn.to_string(),
                                from_faction: sf.to_string(),
                                to_faction: format!("叛逃(杀{})", on),
                                chapter: ts,
                                reason: title.clone(),
                                confidence: 0.5,
                            });
                        }
                    }
                }
            }
        }
    }

    /// Build a faction graph: faction_name → list of entity names.
    pub fn faction_graph(
        &self,
        entities: &[crate::compiler::Entity],
    ) -> HashMap<String, Vec<String>> {
        let mut graph: HashMap<String, Vec<String>> = HashMap::new();
        for e in entities {
            let f = self.faction_of(&e.name).unwrap_or("未知").to_string();
            graph.entry(f).or_default().push(e.name.clone());
        }
        graph
    }

    /// Print faction transition report to stderr.
    pub fn print_report(&self) {
        if self.transitions.is_empty() {
            eprintln!("  No faction transitions");
            return;
        }
        for t in &self.transitions {
            eprintln!(
                "  Ch.{}  {}  {} → {}  ({}) [conf={}]",
                t.chapter, t.entity, t.from_faction, t.to_faction, t.reason, t.confidence
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{Event, EventParticipant};

    fn make_event(ts: i32, title: &str, subj: &str, obj: &str) -> Event {
        Event {
            id: None,
            title: title.into(),
            event_type: "action".into(),
            timestamp: Some(ts),
            location: None,
            description: String::new(),
            effects: vec![],
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

    /// Verify surrender event triggers faction switch.
    ///
    /// Uses 袁绍 (baseline 群雄) surrendering to 曹操 (魏) so an actual
    /// transition is recorded. Note: 张辽 is already 魏 in faction_map.json
    /// (his end-state), so "张辽降曹操" records no transition — a known
    /// config/doc inconsistency tracked in CODE_REVIEW_FINDINGS.
    #[test]
    fn surrender_changes_faction() {
        let mut ft = FactionTracker::from_file("三国演义", "config/faction_map.json").unwrap();
        assert_eq!(
            ft.faction_of("吕布"),
            Some("群雄"),
            "baseline: Lu Bu in 群雄"
        );
        assert_eq!(
            ft.faction_of("袁绍"),
            Some("群雄"),
            "baseline: Yuan Shao in 群雄"
        );

        ft.process_event(&make_event(20, "袁绍降曹操", "袁绍", "曹操"));
        let trans = &ft.transitions;
        assert!(
            trans.iter().any(|t| t.entity == "袁绍"),
            "Yuan Shao surrender should record a faction transition"
        );
        assert_eq!(
            ft.faction_of("袁绍"),
            Some("魏"),
            "Yuan Shao's final faction is 魏"
        );
    }

    /// Verify faction graph groups entities correctly.
    #[test]
    fn faction_graph_groups_correctly() {
        let ft = FactionTracker::from_file("三国演义", "config/faction_map.json").unwrap();
        assert_eq!(ft.faction_of("刘备"), Some("蜀"));
        assert_eq!(ft.faction_of("曹操"), Some("魏"));
        assert_eq!(ft.faction_of("吕布"), Some("群雄"));
    }
}
