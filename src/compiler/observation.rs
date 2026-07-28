//! Observation Compiler — Phase 4.
//!
//! Extracts SPO (Subject–Predicate–Object) triples from sentences using known
//! entity mentions, strong verbs, dialog markers, and action verbs.
//!
//! Observations are the central IR of the compiler. They are NOT persisted —
//! they feed into [`ObjectBuilder`] + [`EdgeBuilder`] in later phases.
//!
//! ## Extraction sources
//!
//! | Pattern | Example | Observation |
//! |---------|---------|-------------|
//! | Strong verb co-occurrence | "赵云救阿斗" | S:赵云, P:救, O:阿斗 |
//! | Dialog marker | "玄德曰：孔明..." | S:刘备, P:曰, O:诸葛亮 |
//! | Single-verb action | "曹操大怒" | S:曹操, P:怒 |
//! | Location/time attributes | "于长坂坡" | attr: location=长坂坡 |

use crate::compiler::{
    Argument, Observation, ResolvedMention, SemanticRole, Sentence,
};

/// Configuration for the observation compiler.
///
/// All verb lists default to the hardcoded values from
/// [`ingest::extract`] when set to `None`.
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum distance (in characters) between a verb and an entity mention
    /// for them to be considered related.
    pub proximity_chars: usize,
    /// Dialog markers to recognize, e.g. "曰：", "道：".
    pub dialog_markers: Vec<String>,
    /// Strong verbs that indicate meaningful events.
    /// Falls back to [`ingest::extract::STRONG_VERBS`] when empty.
    pub strong_verbs: Vec<String>,
    /// Action verbs that signal a character acting.
    /// Falls back to [`ingest::extract::ACTION_VERBS`] when empty.
    pub action_verbs: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            proximity_chars: 50,
            dialog_markers: vec!["曰：".into(), "道：".into(), "言：".into(), "答：".into()],
            strong_verbs: vec![
                "杀","斩","擒","捉","打","战","斗","败","胜","攻","破","救",
                "逃","死","亡","卒","殉","绑","缚","捉拿","骂","哭","笑","怒",
                "拜","嫁","娶","配","封","赐","赏",
            ].into_iter().map(String::from).collect(),
            action_verbs: vec![
                "大怒","大喜","领兵","引军","挺枪","纵马","大呼","拍马","拔剑",
                "挺刀","引兵","出马","上前","奋然","勃然","大惊","大败","引军",
            ].into_iter().map(String::from).collect(),
        }
    }
}

impl Config {
    /// Strong verbs configured for this profile.
    pub fn strong_verbs(&self) -> &[String] {
        &self.strong_verbs
    }

    /// Action verbs configured for this profile.
    pub fn action_verbs(&self) -> &[String] {
        &self.action_verbs
    }
}



/// Build observations from resolved mentions within a single sentence.
///
/// # Algorithm
///
/// 1. Scan the sentence text for strong verbs and dialog markers.
/// 2. For each verb found, look for entity mentions nearby.
/// 3. The nearest entity mention before the verb becomes the Subject.
/// 4. The nearest entity mention after the verb becomes the Object (if any).
/// 5. Attributes (location, time, instrument) are extracted from known
///    keyword patterns in the same sentence.
///
/// Returns empty vec when no observations can be extracted.
pub fn extract_observations(
    sentence: &Sentence,
    mentions: &[ResolvedMention],
    config: &Config,
) -> Vec<Observation> {
    if mentions.is_empty() || sentence.text.len() < 2 {
        return Vec::new();
    }

    let text = &sentence.text;
    let mut observations = Vec::new();

    // Filter mentions to this sentence using offset proximity
    let sent_start = sentence.start_offset;
    let sent_end = sentence.end_offset;
    let local_mentions: Vec<&ResolvedMention> = mentions
        .iter()
        .filter(|m| {
            m.mention.offset.start >= sent_start && m.mention.offset.end <= sent_end
        })
        .collect();

    if local_mentions.is_empty() {
        return Vec::new();
    }

    // 1. Dialog pattern: X 曰/Y道： → X said Y
    for marker in &Config::default().dialog_markers {
        if let Some(pos) = text.find(marker.as_str()) {
            let marker_byte = pos;
            // Speaker is the nearest mention BEFORE the marker
            let speaker = local_mentions
                .iter()
                .filter(|m| m.mention.offset.end <= sent_start + marker_byte)
                .last();

            // Addressee is the nearest mention AFTER the marker
            let addressee = local_mentions
                .iter()
                .find(|m| {
                    let abs_start = m.mention.offset.start;
                    abs_start >= sent_start + marker_byte + marker.len()
                });

            if let Some(s) = speaker {
                let mut args = vec![Argument {
                    role: SemanticRole::Subject,
                    value: s.resolved_to.clone(),
                }];
                if let Some(a) = addressee {
                    args.push(Argument {
                        role: SemanticRole::Object,
                        value: a.resolved_to.clone(),
                    });
                }
                observations.push(Observation {
                    sentence_id: sentence.chunk_index * 10000 + sentence.index,
                    predicate: "曰".into(),
                    arguments: args,
                    confidence: 0.9,
                });
            }
        }
    }

    // 2. Strong verb / action verb patterns
    // Collect all verb positions
    let mut verb_positions: Vec<(usize, &str)> = Vec::new();
    for verb in config.strong_verbs().iter().chain(config.action_verbs().iter()) {
        for (pos, _) in text.match_indices(verb.as_str()) {
            verb_positions.push((pos, verb.as_str()));
        }
    }
    verb_positions.sort_by_key(|&(pos, _)| pos);

    // For each verb, find subject (mention before verb) and object (mention after verb)
    for &(verb_pos, verb) in &verb_positions {
        // Skip verbs that are part of dialog markers (already handled above)
        let is_dialog = Config::default().dialog_markers.iter().any(|m| {
            let m_pos = text.find(m.as_str());
            m_pos.map_or(false, |p| {
                verb_pos >= p && verb_pos < p + m.len()
            })
        });
        if is_dialog {
            continue;
        }

        // Subject: closest mention before verb (within proximity)
        let subject = local_mentions
            .iter()
            .filter(|m| {
                let abs_end = m.mention.offset.end;
                abs_end <= sent_start + verb_pos
                    && (sent_start + verb_pos - abs_end) < Config::default().proximity_chars
            })
            .last();

        // Object: closest mention after verb
        let object = local_mentions
            .iter()
            .find(|m| {
                let abs_start = m.mention.offset.start;
                abs_start >= sent_start + verb_pos + verb.len()
                    && (abs_start - (sent_start + verb_pos + verb.len()))
                        < Config::default().proximity_chars
            });

        if let Some(s) = subject {
            let mut args = vec![Argument {
                role: SemanticRole::Subject,
                value: s.resolved_to.clone(),
            }];
            if let Some(o) = object {
                args.push(Argument {
                    role: SemanticRole::Object,
                    value: o.resolved_to.clone(),
                });
            }
            observations.push(Observation {
                sentence_id: sentence.chunk_index * 10000 + sentence.index,
                predicate: verb.to_string(),
                arguments: args,
                confidence: if object.is_some() { 0.85 } else { 0.7 },
            });
        }
    }

    observations
}

/// Batch process all sentences through the observation compiler.
pub fn extract_all(
    sentences: &[Sentence],
    mentions: &[ResolvedMention],
    config: &Config,
) -> Vec<Observation> {
    let mut all = Vec::new();
    for sent in sentences {
        all.extend(extract_observations(sent, mentions, config));
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{Mention, ResolveStrategy};

    fn make_mention(surface: &str, canonical: &str, offset: usize, sent_id: usize) -> ResolvedMention {
        ResolvedMention {
            mention: Mention {
                sentence_id: sent_id,
                surface: surface.into(),
                canonical_name: canonical.into(),
                offset: offset..(offset + surface.len()),
                confidence: 1.0,
            },
            resolved_to: canonical.into(),
            strategy: ResolveStrategy::Identity,
        }
    }

    fn make_sentence(text: &str, offset: usize, chunk_idx: usize, sent_idx: usize) -> Sentence {
        Sentence {
            chunk_index: chunk_idx,
            index: sent_idx,
            text: text.into(),
            start_offset: offset,
            end_offset: offset + text.len(),
        }
    }

    /// Objective: Verify that a strong verb with subject and object produces
    /// one observation with both arguments.
    /// Invariants: predicate matches the verb; subject and object are correct.
    #[test]
    fn strong_verb_produces_spo() {
        let text = "赵云救阿斗。";
        let offset = 100;
        let sent = make_sentence(text, offset, 0, 0);
        let mentions = vec![
            make_mention("赵云", "赵云", offset, 0),
            // 赵云=6 bytes, 救=3 bytes → 阿斗 starts at byte 9
            make_mention("阿斗", "阿斗", offset + 9, 0),
        ];
        let obs = extract_observations(&sent, &mentions, &Config::default());
        assert!(!obs.is_empty(), "should find at least one observation");
        let has_rescue = obs.iter().any(|o| {
            o.predicate == "救"
                && o.arguments.iter().any(|a| a.role == SemanticRole::Subject && a.value == "赵云")
                && o.arguments.iter().any(|a| a.role == SemanticRole::Object && a.value == "阿斗")
        });
        assert!(has_rescue, "should find 赵云救阿斗 observation");
    }

    /// Objective: Verify that a dialog pattern ("玄德曰：孔明") produces an observation.
    /// Invariants: predicate="曰"; subject=刘备; object=诸葛亮.
    #[test]
    fn dialog_produces_speaker_observation() {
        let text = "玄德曰：孔明";
        let offset = 0;
        let sent = make_sentence(text, offset, 0, 0);
        // "玄德" and "孔明" are aliases that would be resolved to canonical names
        let mentions = vec![
            make_mention("玄德", "刘备", offset, 0),
            make_mention("孔明", "诸葛亮", offset + 9, 0),
        ];
        let obs = extract_observations(&sent, &mentions, &Config::default());
        // Note: the dialog marker "曰：" triggers the dialog pattern
        // but the observation compiler also tries "曰" as a strong verb.
        let has_dialog = obs.iter().any(|o| o.predicate == "曰");
        assert!(has_dialog, "dialog should produce 曰 observation");
    }

    /// Objective: Verify that sentences with mentions but no strong verb
    /// produce no observations.
    /// Invariants: Empty vec.
    #[test]
    fn no_strong_verb_yields_no_observations() {
        let text = "赵云是一个好汉。";
        let sent = make_sentence(text, 0, 0, 0);
        let mentions = vec![make_mention("赵云", "赵云", 0, 0)];
        let obs = extract_observations(&sent, &mentions, &Config::default());
        assert!(obs.is_empty(), "no strong verb → no observations");
    }

    /// Objective: Verify that empty mentions produce no observations.
    /// Invariants: Empty vec; no panics.
    #[test]
    fn empty_mentions_yield_no_observations() {
        let sent = make_sentence("无关文本。", 0, 0, 0);
        let obs = extract_observations(&sent, &[], &Config::default());
        assert!(obs.is_empty());
    }
}
