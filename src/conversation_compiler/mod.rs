use std::collections::HashSet;

use crate::classifier::MemoryClassifier;
use crate::cognition::{Fact, FactType, Mention, Observation, Rule};
use crate::extractor::{ExperienceExtractor, ExtractorConfig};
use crate::filter::NoiseFilter;
use crate::observation_compiler::DefaultRule;
use crate::scorer::ImportanceScorer;
use crate::types::{
    CompiledConversation, Decision, Memory, MemoryType, Message, ReasoningStep, SessionState,
};

mod markers;

use markers::{FUNCTIONAL_MATCHER, OBSERVATION_MARKERS};

pub use markers::{MarkerReport, inspect_markers};

static MODULE_NAMES: &[&str] = &[
    "compiler",
    "distiller",
    "store",
    "detector",
    "prompt",
    "classifier",
    "scorer",
    "extractor",
    "filter",
    "resolver",
    "embed",
    "retrieval",
    "mcp",
    "types",
    "config",
    "error",
];

pub struct ConversationCompiler {
    extractor: ExperienceExtractor,
    classifier: MemoryClassifier,
    scorer: ImportanceScorer,
    filter: NoiseFilter,
}

impl ConversationCompiler {
    pub fn new() -> Self {
        Self {
            extractor: ExperienceExtractor::new(ExtractorConfig {
                enable_cross_turn: true,
            }),
            classifier: MemoryClassifier::new(),
            scorer: ImportanceScorer::new(),
            filter: NoiseFilter::new(),
        }
    }

    pub fn compile(&self, tenant_id: &str, messages: &[Message]) -> CompiledConversation {
        let mut knowledge: Vec<Memory> = Vec::new();
        let mut decisions: Vec<Decision> = Vec::new();
        let mut session = SessionState::default();
        let mut seen_files = HashSet::new();
        let mut unresolved_problems: Vec<String> = Vec::new();

        // Pass 1: extract reasoning chain from structured Message fields.
        // Pattern: user(trigger) → assistant(tool_invocation) → tool(status) → assistant(reasoning)
        for (i, msg) in messages.iter().enumerate() {
            // Look for a user message that is followed by a tool invocation
            if !msg.is_user() {
                continue;
            }
            let next = messages.get(i + 1);
            let inv = match next.and_then(|m| m.tool_invocation.as_ref()) {
                Some(inv) => inv,
                None => continue,
            };
            // Find the tool result that follows
            let rest = &messages[i + 2..];
            let tool_result = rest.iter().find(|m| m.tool_call_id.is_some());
            let status = match tool_result {
                Some(r) if r.content.contains("error") || r.content.contains("failed") => "error",
                Some(_) => "ok",
                None => "timeout",
            };
            // Find the assistant response after the tool result
            let after_result = rest
                .iter()
                .skip_while(|m| m.tool_call_id.is_none())
                .skip(1)
                .find(|m| m.is_assistant());
            let reasoning = after_result.map(|m| m.content.clone()).unwrap_or_default();
            session.reasoning_chain.push(ReasoningStep {
                trigger: msg.content.clone(),
                tool_name: inv.name.clone(),
                tool_args: inv.arguments.clone(),
                status: status.to_string(),
                reasoning,
            });
        }
        // Keep only the last 5 reasoning steps
        if session.reasoning_chain.len() > 5 {
            session.reasoning_chain = session
                .reasoning_chain
                .split_off(session.reasoning_chain.len() - 5);
        }

        // Pass 2: detect session state from user messages.
        for msg in messages {
            if !msg.is_user() {
                continue;
            }
            let c = &msg.content;

            // Goal: first substantive user message that isn't a greeting
            if session.current_goal.is_empty()
                && c.len() > 15
                && !c.to_lowercase().starts_with("thanks")
                && !c.starts_with("好的")
            {
                let goal: String = c.chars().take(100).collect();
                session.current_goal = goal;
            }

            // Files: scan for known extensions
            for word in c.split(|c: char| {
                c.is_whitespace()
                    || c == '，'
                    || c == '。'
                    || c == '、'
                    || c == '？'
                    || c == '?'
                    || c == '）'
                    || c == '('
                    || c == ')'
                    || c == ','
                    || c == ';'
            }) {
                let cleaned: String = word
                    .chars()
                    .filter(|c| {
                        c.is_ascii_alphanumeric()
                            || *c == '.'
                            || *c == '_'
                            || *c == '-'
                            || *c == '/'
                    })
                    .collect();
                if cleaned.len() >= 4
                    && !seen_files.contains(&cleaned)
                    && [".rs", ".go", ".ts", ".py", ".toml", ".json", ".yaml", ".md"]
                        .iter()
                        .any(|ext| cleaned.ends_with(ext))
                {
                    seen_files.insert(cleaned.clone());
                    session.current_files.push(cleaned);
                }
            }
        }

        // Module: scan all messages for known names
        session.current_module = messages
            .iter()
            .filter_map(|m| {
                MODULE_NAMES
                    .iter()
                    .find(|&&mod_name| m.content.contains(mod_name))
                    .copied()
            })
            .next()
            .unwrap_or("")
            .to_string();

        // Open problems: user messages that got no assistant reply
        let answered: HashSet<&str> = messages
            .windows(2)
            .filter(|w| w[0].is_user() && w[1].is_assistant())
            .map(|w| w[0].content.as_str())
            .collect();
        for msg in messages {
            if msg.is_user() && !answered.contains(msg.content.as_str()) {
                unresolved_problems.push(msg.content.clone());
            }
        }
        session.open_problems = unresolved_problems.iter().take(5).cloned().collect();

        // Pass 2: extract knowledge via pipeline
        let filtered: Vec<Message> = messages
            .iter()
            .filter(|m| !self.filter.is_noise(m))
            .cloned()
            .collect();
        let raw_pairs = self.extractor.extract(&filtered);
        for raw in &raw_pairs {
            let memory_type = self.classifier.classify(&raw.problem, &raw.solution);
            let importance = self.scorer.score(&raw.problem, &raw.solution, memory_type);

            if memory_type == MemoryType::Knowledge && importance >= 0.3 {
                let mut mem = Memory::new(tenant_id, memory_type, &raw.solution, importance);
                mem.summary = if raw.problem.is_empty() {
                    raw.solution.clone()
                } else {
                    format!("{}：{}", raw.problem, raw.solution)
                };
                knowledge.push(mem);
            }

            // Decision detection: solution mentions an action was taken.
            // No bare `已`: it appears inside fillers ("已经确认") and made
            // nearly every solution a decision (mirrors the agent_facts
            // COMPLETION_MARKERS fix); only multi-char completion phrases
            // count.
            let sol_lower = raw.solution.to_lowercase();
            let is_decision = sol_lower.contains("done")
                || sol_lower.contains("implemented")
                || sol_lower.contains("replaced")
                || sol_lower.contains("完成");
            let prob_lower = raw.problem.to_lowercase();
            let has_replacement = prob_lower.contains("replace")
                || prob_lower.contains("换成")
                || prob_lower.contains("改用");

            if is_decision || has_replacement {
                let module = raw
                    .problem
                    .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
                    .filter(|w| !w.is_empty())
                    .find(|w| MODULE_NAMES.contains(w))
                    .unwrap_or("general")
                    .to_string();
                decisions.push(Decision {
                    decision: raw.problem.clone(),
                    rationale: raw.solution.clone(),
                    module,
                    importance: if is_decision { 0.9 } else { 0.7 },
                });
            }
        }

        decisions.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        session.recent_decisions = decisions
            .iter()
            .take(3)
            .map(|d| d.decision.clone())
            .collect();

        // Dedup + sort knowledge
        let mut seen = HashSet::new();
        knowledge.retain(|mem| {
            let key = if let Some((p, _)) = mem.summary.split_once('：') {
                p.to_string()
            } else {
                mem.summary.chars().take(40).collect()
            };
            seen.insert(key)
        });
        knowledge.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        CompiledConversation {
            knowledge,
            decisions,
            session,
        }
    }
}

impl Default for ConversationCompiler {
    fn default() -> Self {
        Self::new()
    }
}

/// Compile user-authored messages through the unified Observation IR.
///
/// The legacy conversation result remains unchanged; these observations are an
/// additive event-sourced view used by the cognition store.
pub fn compile_user_observations(messages: &[Message], user_entity_id: i64) -> Vec<Observation> {
    let subject = Mention {
        entity_id: Some(user_entity_id),
        surface: "User".to_string(),
        canonical_name: "User".to_string(),
    };
    // Marker → action verb. The marker set must cover the common
    // preference/emotion/goal/plan vocabulary (ZH + EN) so ordinary
    // conversations produce facts — previously only 12 words were matched and
    // a message like "我很焦虑，压力很大" compiled ZERO observations, so the
    // facts table stayed empty and persona_check had nothing to query.
    //
    // Entries are deliberately multi-character where a single character would
    // false-positive inside unrelated words: "累" matches 积累/劳累, "困"
    // matches 困难, "想" matches 想象/想法 — so we use 好累/困倦/想念/想学
    // etc. instead. Each marker is matched with `find`, so a longer marker is
    // always safe.
    // Marker → action verb pairs come from `config/markers_zh.json` +
    // `config/markers_en.json` plus the user's own `config/*.user.json`
    // (loaded once via OBSERVATION_MARKERS, editable without recompiling),
    // falling back to the built-in safety net when no file can be read.
    // See `markers.rs` for the merge order, `_remove` and validation.
    let actions = OBSERVATION_MARKERS.pairs.as_slice();

    messages
        .iter()
        .filter(|message| message.is_user())
        .flat_map(|message| {
            let subject = subject.clone();
            // Collect the functional cues (negation / uncertainty) ONCE per
            // message using the pre-built functional-word matcher (P3).
            // Functional words live in `config/dictionary.json`
            // (ELITE_LEXICON_PLAN §5.3). Negation is then resolved per MARKER —
            // see `negation_near` for why a message-level flag is too blunt.
            let cues: Vec<crate::lexicon::LexiconMatch> =
                FUNCTIONAL_MATCHER.find_iter(&message.content).collect();
            let uncertain = cues.iter().any(|m| m.semantic_class == "uncertainty");

            // Collect every marker hit first, then emit ONE observation per
            // action. A sentence matching several same-action markers
            // (失眠+加班+压力+好累 → four feel markers) must yield a SINGLE
            // feel observation — otherwise every near-identical fact pollutes
            // the cognitive snapshot with repeated emotion rows.
            let mut seen_actions: Vec<&str> = Vec::new();
            let mut hits: Vec<(usize, &str, &str)> = Vec::new(); // (offset, action, marker)
            for (marker, action) in actions {
                if let Some(offset) = message.content.find(marker) {
                    if !seen_actions.contains(&action.as_str()) {
                        seen_actions.push(action.as_str());
                        hits.push((offset, action.as_str(), marker.as_str()));
                    }
                }
            }

            hits.into_iter().map(move |(offset, action, marker)| {
                let mut modifiers = vec![crate::cognition::Modifier {
                    key: "content".to_string(),
                    value: message.content.clone(),
                }];
                // A `dislike` marker IS the negative preference ("我讨厌应酬"):
                // tag it negated even when no clause-level 不/没 cue is present,
                // otherwise it compiles as an affirmative preference (the
                // exact opposite stance) and StanceFlip never fires.
                if action == "dislike"
                    || negation_near(&message.content, offset, marker.len(), &cues)
                {
                    modifiers.push(crate::cognition::Modifier {
                        key: "negated".to_string(),
                        value: "true".to_string(),
                    });
                }
                if uncertain {
                    modifiers.push(crate::cognition::Modifier {
                        key: "uncertain".to_string(),
                        value: "true".to_string(),
                    });
                }
                Observation {
                    subject: subject.clone(),
                    action: action.to_string(),
                    object: None,
                    modifiers,
                    timestamp: None,
                    evidence: Some(crate::cognition::EvidenceRef {
                        doc_id: 0,
                        offset,
                        length: marker.len(),
                        text: message.content.clone(),
                    }),
                }
            })
        })
        .collect()
}

/// Characters that end a clause. A negation cue on the far side of one belongs
/// to a different statement and must not negate this marker.
const CLAUSE_BREAKS: &[char] = &[
    '，', '。', '！', '？', '；', '、', '：', ',', '.', '!', '?', ';', ':',
];

/// How many characters after a marker still count as "immediately negated"
/// ("开心不起来").
const NEGATION_WINDOW_CHARS: usize = 3;

/// True when the marker at `offset` is negated **in its own clause**.
///
/// A message-level negation flag is too blunt. "想学吉他很久了，一直在纠结买不买"
/// and "特别想去海边住一段时间，什么都不干" both contain a cue, yet their plan is
/// affirmative — flagging the whole message marked those goals negated, and
/// negated goals are dropped (ELITE_LEXICON_PLAN §13.3), so the plans silently
/// disappeared. A cue only counts when it sits before the marker with no clause
/// break in between, or within [`NEGATION_WINDOW_CHARS`] characters after it —
/// the same rule `src/commitment.rs` applies to promises.
fn negation_near(
    content: &str,
    offset: usize,
    marker_len: usize,
    cues: &[crate::lexicon::LexiconMatch],
) -> bool {
    let before = |cue: &crate::lexicon::LexiconMatch| {
        cue.semantic_class == "negation"
            && cue.end <= offset
            && !content[cue.end..offset].contains(CLAUSE_BREAKS)
    };
    if cues.iter().any(before) {
        return true;
    }
    let after_start = offset + marker_len;
    let window: String = content[after_start..]
        .chars()
        .take_while(|c| !CLAUSE_BREAKS.contains(c))
        .take(NEGATION_WINDOW_CHARS)
        .collect();
    cues.iter().any(|cue| {
        cue.semantic_class == "negation"
            && cue.start >= after_start
            && cue.start < after_start + window.len()
    })
}

/// Convert precompiled user observations into immutable facts.
///
/// A negated statement is a *state*, not a non-event: "我不喜欢应酬" becomes a
/// Preference fact tagged `negated: true`. Dropping the observation instead left
/// the engine unable to represent a user's negative stance at all, and made
/// `StanceFlip` (喜欢 X → 不喜欢 X) structurally unreachable for a user entity —
/// negated facts only ever existed for the agent's own persona.
///
/// The same holds for [`FactType::Goal`]: "我不打算考公务员了" is kept as a
/// **negated** goal. ELITE_LEXICON_PLAN §13.3 forbids it surfacing as an
/// *affirmative* goal, and that is guaranteed by the flag plus
/// `StateEngine::aggregate` (which lists current state from affirmative facts
/// only) — not by discarding the fact, which lost the change entirely. The
/// history layer then reports the real `stance_flip` on the goal dimension.
///
/// Whether the statement was negated is decided per marker in its own clause
/// (`negation_near`), never by a message-wide flag. Uncertain statements are kept
/// but tagged.
pub fn user_facts_from_observations(observations: &[Observation], time: i32) -> Vec<Fact> {
    let rule = DefaultRule;
    observations
        .iter()
        .flat_map(|observation| {
            let negated = has_modifier(observation, "negated");
            let mut facts = rule.apply(observation);
            for fact in &mut facts {
                fact.time = time;
                fact.created_at = i64::from(time);
                if let Some(content) = observation
                    .modifiers
                    .iter()
                    .find(|modifier| modifier.key == "content")
                {
                    fact.payload["content"] = serde_json::Value::String(content.value.clone());
                }
                // Written for BOTH stances, like the agent/persona channels do:
                // a stance is only readable as a stance when the affirmative
                // side declares `negated: false`, and `StanceFlip` detection
                // needs a flag on each side of the window.
                fact.payload["negated"] = serde_json::Value::Bool(negated);
                if has_modifier(observation, "uncertain") {
                    fact.payload["uncertain"] = serde_json::Value::Bool(true);
                }
                // Propagate observation evidence into the fact payload so
                // provenance (source message offset, length, text) is not lost.
                if let Some(ref evidence) = observation.evidence {
                    fact.payload["evidence"] = serde_json::json!({
                        "doc_id": evidence.doc_id,
                        "offset": evidence.offset,
                        "length": evidence.length,
                        "text": evidence.text,
                    });
                }
            }
            facts
        })
        .collect()
}

/// Return `true` if the observation carries the given modifier key.
fn has_modifier(observation: &Observation, key: &str) -> bool {
    observation
        .modifiers
        .iter()
        .any(|modifier| modifier.key == key && modifier.value == "true")
}

/// Compile user messages through the Observation IR into immutable facts.
///
/// Two channels contribute, because they answer different questions and neither
/// can substitute for the other:
///
/// - the **observation marker** table answers "what does the user feel / want /
///   dislike" and can only produce preference / goal / emotion / event;
/// - [`crate::self_disclosure`] answers "who is this person" (name, age,
///   occupation, city, family, pets, interests, habits) — the types the marker
///   table can never emit.
pub fn compile_user_facts(messages: &[Message], user_entity_id: i64, time: i32) -> Vec<Fact> {
    let observations = compile_user_observations(messages, user_entity_id);
    user_facts_from_channels(&observations, messages, user_entity_id, time)
}

/// Turn precompiled observations **and the messages they came from** into facts.
///
/// Both inputs are required: the marker table reads observations, while the
/// self-disclosure channel reads the raw message text. Keeping the two channels
/// behind one entry point is what stops a caller from wiring only half the
/// pipeline — `CognitionCompiler::compile_conversation` used to inline
/// `compile_user_observations` + `user_facts_from_observations` and silently
/// bypassed the self-disclosure channel on the `memory_compile` path.
#[must_use]
pub fn user_facts_from_channels(
    observations: &[Observation],
    messages: &[Message],
    user_entity_id: i64,
    time: i32,
) -> Vec<Fact> {
    let mut facts = user_facts_from_observations(observations, time);
    facts.extend(crate::self_disclosure::disclosures_from_messages(
        messages,
        user_entity_id,
        time,
    ));
    facts
}

/// Convert conversation memories into User Model Facts.
///
/// Bridges the Memory Distillation pipeline (memories) with the Cognition
/// Engine's User model (Facts). Each memory becomes a User Fact typed
/// according to its MemoryType.
pub fn user_facts_from_memories(memories: &[Memory]) -> Vec<Fact> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    memories
        .iter()
        .filter(|m| m.importance > 0.3)
        .enumerate()
        .map(|(i, m)| {
            let fact_type = match m.memory_type {
                MemoryType::Preference => FactType::Preference,
                MemoryType::Skill => FactType::Interest,
                MemoryType::Experience => FactType::Event,
                MemoryType::Profile => FactType::Identity,
                MemoryType::Knowledge => FactType::Interest,
                _ => FactType::Event,
            };
            Fact {
                id: None,
                entity_id: 0,
                fact_type,
                time: (now - i as i64) as i32,
                payload: serde_json::json!({
                    "content": m.content,
                    "summary": m.summary,
                    "confidence": m.importance,
                }),
                evidence_id: None,
                created_at: now,
                ..Fact::default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bench_tests {
    use super::*;
    use crate::types::*;

    #[test]
    fn no_tool_invocation_fields_yields_empty() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "hello"),
            Message::new("assistant", "hi"),
        ];
        let result = compiler.compile("t1", &msgs);
        assert!(result.session.reasoning_chain.is_empty());
    }
}
