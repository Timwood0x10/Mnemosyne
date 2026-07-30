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

        // If no verb matched, try dialog markers
        if !found {
            for marker in &["说", "曰", "said", "replied", "asked"] {
                if let Some(pos) = sentence.find(marker) {
                    let speaker = find_subject(sentence, pos, resolve_mention);
                    if let Some(s) = speaker {
                        observations.push(Observation {
                            subject: s,
                            action: marker.to_string(),
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
    // Try sliding windows of 2-6 chars before the position
    // Use char_indices to avoid UTF-8 byte boundary issues
    let char_offset = text[..pos].chars().count();
    let start_char = char_offset.saturating_sub(4);
    let before: String = text
        .chars()
        .skip(start_char)
        .take(char_offset - start_char)
        .collect();
    for len in (2..=6).rev() {
        if len > before.chars().count() {
            continue;
        }
        let candidate: String = before
            .chars()
            .rev()
            .take(len)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
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
    let after = &text[start..];
    for len in (2..=6).rev() {
        if len > after.chars().count() {
            continue;
        }
        let candidate: String = after.chars().take(len).collect();
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

        // Determine fact type from verb
        let fact_type = match verb.as_str() {
            v if ["杀", "斩", "kill", "murder", "attack"].contains(&v) => FactType::Event,
            v if ["喜欢", "love", "like", "prefer"].contains(&v) => FactType::Preference,
            v if ["想", "要", "打算", "准备", "want", "plan", "prepare"].contains(&v) => {
                FactType::Goal
            }
            v if ["觉得", "感觉", "feel", "stress", "tired"].contains(&v) => FactType::Emotion,
            v if ["在", "是", "be", "work", "live"].contains(&v) => FactType::Occupation,
            v if ["有", "have", "own", "belong"].contains(&v) => FactType::Identity,
            _ => FactType::Event,
        };

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
            time: observation.timestamp.unwrap_or(0),
            payload,
            evidence_id: None,
            created_at: 0,
        }]
    }
}
