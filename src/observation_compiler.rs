//! Observation Compiler — converts text into Observations, then Facts.
//!
//! Reuses existing extract.rs infrastructure (Aho-Corasick verb matching,
//! sentence splitting, mention scanning) under the unified Observation IR.
//!
//! ## Pipeline
//!
//! ```text
//! Text → Sentence Splitter → Mention Scan → Verb Match → Observation → Rule → Fact
//! ```
//!
//! Every step is language-agnostic. Language-specific behaviour comes from
//! the [`LanguageFrontend`] trait.

use aho_corasick::AhoCorasick;

use crate::cognition::{Fact, FactType, Mention, Observation, Rule};

/// Compile text into Observations (universal IR).
///
/// # Parameters
///
/// * `text` — raw text (novel, conversation, email, etc.)
/// * `sentences` — pre-split sentences
/// * `verbs` — verb patterns for action detection
/// * `resolve_mention` — a function that resolves a text span to an entity mention
///
/// # Returns
///
/// A list of Observations extracted from the text.
pub fn compile_observations(
    sentences: &[&str],
    verbs: &[String],
    resolve_mention: &dyn Fn(&str) -> Option<Mention>,
) -> Vec<Observation> {
    let mut observations = Vec::new();

    // Build Aho-Corasick automaton once (reusing the extract.rs optimisation)
    if verbs.is_empty() {
        return observations;
    }
    let verb_refs: Vec<&str> = verbs.iter().map(|s| s.as_str()).collect();
    let ac = match AhoCorasick::new(&verb_refs) {
        Ok(ac) => ac,
        Err(_) => return observations,
    };

    for sentence in sentences {
        if sentence.len() < 3 {
            continue;
        }

        // Try verb matching (same as extract.rs but produces Observations)
        let mut found = false;
        for m in ac.find_iter(sentence) {
            let verb = verb_refs[m.pattern()];
            let pos = m.start();

            // Find subject (mention before the verb)
            let subject = find_subject(sentence, pos, resolve_mention);
            // Find object (mention after the verb)
            let object = find_object(sentence, pos + verb.len(), resolve_mention);

            observations.push(Observation {
                subject: subject.unwrap_or_else(|| Mention {
                    entity_id: None,
                    surface: String::new(),
                    canonical_name: String::new(),
                }),
                action: verb.to_string(),
                object,
                modifiers: Vec::new(),
                timestamp: None,
                evidence: None,
            });
            found = true;
        }

        // If no verb matched, try dialog markers (speech-class lexemes from
        // the lexicon registry — single source of truth, P3).
        if !found {
            let speech_markers: Vec<String> = {
                let guard = crate::lexicon::global();
                guard
                    .by_class("speech")
                    .iter()
                    .map(|lex| lex.lemma.clone())
                    .collect()
            };
            for marker in &speech_markers {
                if let Some(pos) = sentence.find(marker.as_str()) {
                    let speaker = find_subject(sentence, pos, resolve_mention);
                    if let Some(s) = speaker {
                        observations.push(Observation {
                            subject: s,
                            action: marker.clone(),
                            object: None,
                            modifiers: Vec::new(),
                            timestamp: None,
                            evidence: None,
                        });
                    }
                    break;
                }
            }
        }
    }

    observations
}

/// Find the closest entity mention before a given position.
fn find_subject(
    text: &str,
    pos: usize,
    resolve: &dyn Fn(&str) -> Option<Mention>,
) -> Option<Mention> {
    let before: Vec<char> = text[..pos].chars().rev().take(64).collect();
    let before: Vec<char> = before.into_iter().rev().collect();
    for len in 1..=before.len() {
        let candidate: String = before[before.len() - len..]
            .iter()
            .collect::<String>()
            .trim_matches(|character: char| {
                character.is_whitespace() || character.is_ascii_punctuation()
            })
            .to_string();
        if candidate.is_empty() {
            continue;
        }
        if let Some(mention) = resolve(&candidate) {
            return Some(mention);
        }
    }
    None
}

/// Find the closest entity mention after a given position.
fn find_object(
    text: &str,
    start: usize,
    resolve: &dyn Fn(&str) -> Option<Mention>,
) -> Option<Mention> {
    let after: Vec<char> = text[start..].chars().take(64).collect();
    for len in 1..=after.len() {
        let candidate: String = after[..len]
            .iter()
            .collect::<String>()
            .trim_matches(|character: char| {
                character.is_whitespace() || character.is_ascii_punctuation()
            })
            .to_string();
        if candidate.is_empty() {
            continue;
        }
        if let Some(mention) = resolve(&candidate) {
            return Some(mention);
        }
    }
    None
}

/// A default rule that converts verb-based Observations to Facts.
///
/// This is a simple heuristic rule. More sophisticated rules can implement
/// the [`Rule`] trait directly.
pub struct DefaultRule;

impl Rule for DefaultRule {
    fn apply(&self, observation: &Observation) -> Vec<Fact> {
        let verb = &observation.action;

        // Determine fact type from verb using the lexicon registry.
        // Look up the verb's lemma in the registry to find its semantic class
        // and cognitive effects, then map those to a FactType.
        let fact_type = verb_to_fact_type(verb);

        let subject_id = observation.subject.entity_id.unwrap_or(0);
        if subject_id == 0 {
            return vec![];
        }

        let payload = serde_json::json!({
            "action": verb,
            "subject": observation.subject.canonical_name,
            "object": observation.object.as_ref().map(|o| o.canonical_name.as_str()),
        });

        vec![Fact {
            id: None,
            entity_id: subject_id,
            fact_type,
            time: i64::from(observation.timestamp.unwrap_or(0)),
            payload,
            evidence_id: None,
            created_at: 0,
            ..Fact::default()
        }]
    }
}

/// One observation action a marker file may use, and the fact type its
/// observations compile into.
#[derive(Debug, Clone, Copy)]
pub struct ObservationAction {
    /// The bucket name used in `config/markers_*.json`.
    pub name: &'static str,
    /// The fact type produced for observations carrying this action.
    pub fact_type: FactType,
    /// One-line meaning, mirrored by `_meta.actions` in the shipped files.
    pub doc: &'static str,
}

/// Every action a marker file may use — the engine's vocabulary contract.
///
/// The **words** belong to the user (they live in `config/`), the **action
/// names** belong to the engine, because each one decides a fact type. This
/// table is the single source for both halves of that contract:
/// `conversation_compiler::markers` refuses an unknown bucket at load time
/// (previously it silently compiled it into `Event` facts), and
/// [`verb_to_fact_type`] reads the same rows instead of keeping a second list.
pub const OBSERVATION_ACTIONS: &[ObservationAction] = &[
    ObservationAction {
        name: "喜欢",
        fact_type: FactType::Preference,
        doc: "preference (positive)",
    },
    ObservationAction {
        name: "love",
        fact_type: FactType::Preference,
        doc: "preference (strong positive; English alias of 喜欢)",
    },
    ObservationAction {
        name: "like",
        fact_type: FactType::Preference,
        doc: "preference (positive; English alias of 喜欢)",
    },
    ObservationAction {
        name: "prefer",
        fact_type: FactType::Preference,
        doc: "preference (alias of 喜欢)",
    },
    ObservationAction {
        name: "dislike",
        fact_type: FactType::Preference,
        doc: "preference (negative)",
    },
    ObservationAction {
        name: "hate",
        fact_type: FactType::Preference,
        doc: "preference (strong negative; alias of dislike)",
    },
    ObservationAction {
        name: "准备",
        fact_type: FactType::Goal,
        doc: "preparing to do something",
    },
    ObservationAction {
        name: "plan",
        fact_type: FactType::Goal,
        doc: "goal / plan",
    },
    ObservationAction {
        name: "want",
        fact_type: FactType::Goal,
        doc: "wish / desire",
    },
    ObservationAction {
        name: "feel",
        fact_type: FactType::Emotion,
        doc: "emotion / body state",
    },
    ObservationAction {
        name: "belief",
        fact_type: FactType::Event,
        doc: "opinion / belief — stored as an Event fact",
    },
    ObservationAction {
        name: "stuck",
        fact_type: FactType::Event,
        doc: "difficulty / blocked — stored as an Event fact",
    },
    ObservationAction {
        name: "life_event",
        fact_type: FactType::Event,
        doc: "major life event (moved / quit / breakup / married)",
    },
];

/// The fact type `action` produces, or `None` when `action` is not part of
/// [`OBSERVATION_ACTIONS`].
///
/// `None` is what makes a typo in a marker file detectable: the loader drops
/// the bucket and reports it instead of letting it fall through to `Event`.
#[must_use]
pub fn action_fact_type(action: &str) -> Option<FactType> {
    OBSERVATION_ACTIONS
        .iter()
        .find(|candidate| candidate.name == action)
        .map(|candidate| candidate.fact_type)
}

/// Map a verb to a `FactType` by consulting the global lexicon registry.
///
/// Falls back to the hardcoded heuristic mapping when the registry has no
/// entry for the verb — this ensures backward compatibility during migration.
fn verb_to_fact_type(verb: &str) -> FactType {
    // Check the registry first.
    let lexemes: Vec<crate::dictionary::Lexeme> = {
        let guard = crate::lexicon::global();
        guard.lookup(verb).into_iter().cloned().collect()
    };
    if let Some(lex) = lexemes.first() {
        for effect in &lex.effects {
            if effect.effect_type == "fact" {
                return match effect.value.as_str() {
                    "preference" => FactType::Preference,
                    "goal" => FactType::Goal,
                    "emotion" => FactType::Emotion,
                    "knowledge" => FactType::Interest,
                    "identity" | "occupation" => FactType::Identity,
                    _ => FactType::Event,
                };
            }
        }
        // No fact effect found; fall back to semantic class.
        match lex.semantic_class.as_str() {
            "attack" | "rescue" | "movement" | "creation" | "transfer" => FactType::Event,
            "speech" => FactType::Event,
            "cognition" => FactType::Interest,
            "emotion" => FactType::Emotion,
            "intention" => FactType::Goal,
            "state" => FactType::Identity,
            _ => FactType::Event,
        }
    } else {
        // No lexeme. The marker-action vocabulary decides first, so a bucket
        // name always means exactly what `OBSERVATION_ACTIONS` says; only then
        // the open-ended corpus heuristics, whose verbs (杀/斩/在/是/…) are
        // free text and therefore default to `Event`.
        action_fact_type(verb).unwrap_or_else(|| legacy_verb_fact_type(verb))
    }
}

/// The pre-lexicon heuristic table, kept for corpus verbs that are not marker
/// actions (ingest derives verbs straight from prose).
fn legacy_verb_fact_type(verb: &str) -> FactType {
    match verb {
        v if ["杀", "斩", "kill", "murder", "attack"].contains(&v) => FactType::Event,
        // Negative preference action: same bucket as 喜欢, distinguished
        // by the observation's `negated` modifier.
        v if ["喜欢", "love", "like", "prefer", "dislike", "hate"].contains(&v) => {
            FactType::Preference
        }
        v if ["想", "要", "打算", "准备", "want", "plan", "prepare"].contains(&v) => {
            FactType::Goal
        }
        v if ["觉得", "感觉", "feel", "stress", "tired"].contains(&v) => FactType::Emotion,
        v if ["在", "是", "be", "work", "live"].contains(&v) => FactType::Occupation,
        v if ["有", "have", "own", "belong"].contains(&v) => FactType::Identity,
        _ => FactType::Event,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(text: &str) -> Option<Mention> {
        [(1, "Anna"), (2, "Vronsky"), (3, "刘备"), (4, "诸葛亮")]
            .into_iter()
            .find(|(_, name)| text.contains(name))
            .map(|(entity_id, name)| Mention {
                entity_id: Some(entity_id),
                surface: name.to_string(),
                canonical_name: name.to_string(),
            })
    }

    /// Objective: Verify long English mentions survive the universal IR path.
    /// Invariants: Subject and object resolve on opposite sides of the verb.
    #[test]
    fn compiles_english_subject_and_object_without_cjk_length_limits() {
        let observations =
            compile_observations(&["Anna loved Vronsky"], &["loved".to_string()], &resolver);

        assert_eq!(
            observations.len(),
            1,
            "One transitive sentence should emit one observation"
        );
        assert_eq!(
            observations[0].subject.canonical_name, "Anna",
            "English subject should resolve before the action"
        );
        assert_eq!(
            observations[0]
                .object
                .as_ref()
                .map(|mention| mention.canonical_name.as_str()),
            Some("Vronsky"),
            "English object should resolve after the action"
        );
    }

    /// Objective: Verify UTF-8 mention windows do not cross invalid boundaries.
    /// Invariants: Chinese subject and object resolve without panic or truncation.
    #[test]
    fn compiles_chinese_subject_and_object_on_character_boundaries() {
        let observations =
            compile_observations(&["刘备拜访诸葛亮"], &["拜访".to_string()], &resolver);

        assert_eq!(
            observations.len(),
            1,
            "One Chinese action should emit one observation"
        );
        assert_eq!(
            observations[0].subject.canonical_name, "刘备",
            "Chinese subject should remain intact"
        );
        assert_eq!(
            observations[0]
                .object
                .as_ref()
                .map(|mention| mention.canonical_name.as_str()),
            Some("诸葛亮"),
            "Chinese object should remain intact"
        );
    }
}
