use std::collections::HashSet;
use std::sync::LazyLock;

use crate::classifier::MemoryClassifier;
use crate::cognition::{Fact, FactType, Mention, Observation, Rule};
use crate::extractor::{ExperienceExtractor, ExtractorConfig};
use crate::filter::NoiseFilter;
use crate::observation_compiler::DefaultRule;
use crate::scorer::ImportanceScorer;
use crate::types::{
    CompiledConversation, Decision, Memory, MemoryType, Message, ReasoningStep, SessionState,
};

/// Single-pass matcher over all functional lexemes (negation, uncertainty, …).
///
/// Built once per process from the lexicon registry (P3: never rebuild inside
/// the per-message loop). The Aho-Corasick automaton scans each message in one
/// pass instead of O(V) `contains` checks per class.
static FUNCTIONAL_MATCHER: LazyLock<crate::lexicon::LexiconMatcher> =
    LazyLock::new(crate::lexicon::LexiconMatcher::from_global_registry);

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
    let actions = [
        ("喜欢", "喜欢"),
        ("偏好", "喜欢"),
        ("欣赏", "喜欢"),
        ("讨厌", "喜欢"),
        ("love", "love"),
        ("like", "like"),
        ("准备", "准备"),
        ("打算", "打算"),
        ("计划", "plan"),
        ("希望", "plan"),
        ("想要", "want"),
        ("want", "want"),
        ("压力", "feel"),
        ("焦虑", "feel"),
        ("担心", "feel"),
        ("害怕", "feel"),
        ("开心", "feel"),
        ("难过", "feel"),
        ("stress", "feel"),
        ("tired", "feel"),
        ("happy", "feel"),
        ("sad", "feel"),
        ("worry", "feel"),
        ("afraid", "feel"),
    ];

    messages
        .iter()
        .filter(|message| message.is_user())
        .flat_map(|message| {
            let subject = subject.clone();
            // Detect negation/uncertainty in a single pass over the message
            // using the pre-built functional-word matcher (P3). Functional
            // words live in `config/dictionary.json` (ELITE_LEXICON_PLAN §5.3).
            let mut negated = false;
            let mut uncertain = false;
            for m in FUNCTIONAL_MATCHER.find_iter(&message.content) {
                match m.semantic_class.as_str() {
                    "negation" => negated = true,
                    "uncertainty" => uncertain = true,
                    _ => {}
                }
            }

            actions.iter().filter_map(move |(marker, action)| {
                message.content.find(marker).map(|offset| {
                    let mut modifiers = vec![crate::cognition::Modifier {
                        key: "content".to_string(),
                        value: message.content.clone(),
                    }];
                    if negated {
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
                        action: (*action).to_string(),
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
        })
        .collect()
}

/// Convert precompiled user observations into immutable facts.
pub fn user_facts_from_observations(observations: &[Observation], time: i32) -> Vec<Fact> {
    let rule = DefaultRule;
    observations
        .iter()
        // Skip observations whose source message was negated
        // (ELITE_LEXICON_PLAN §13.3: "I do not plan to X" must not produce an
        // affirmative Goal). Uncertain statements are kept but tagged.
        .filter(|observation| !has_modifier(observation, "negated"))
        .flat_map(|observation| {
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
pub fn compile_user_facts(messages: &[Message], user_entity_id: i64, time: i32) -> Vec<Fact> {
    let observations = compile_user_observations(messages, user_entity_id);
    user_facts_from_observations(&observations, time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_extracts_knowledge() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "为什么编译那么慢？"),
            Message::new("assistant", "因为lancedb太重"),
        ];
        let r = compiler.compile("t1", &msgs);
        assert!(!r.knowledge.is_empty());
    }

    #[test]
    fn compile_tracks_files() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![Message::new("user", "修改compiler.rs 和 prompt.rs")];
        let r = compiler.compile("t1", &msgs);
        assert!(r.session.current_files.contains(&"compiler.rs".to_string()));
        assert!(r.session.current_files.contains(&"prompt.rs".to_string()));
    }

    #[test]
    fn compile_first_user_msg_is_goal() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "帮我实现prompt模块"),
            Message::new("assistant", "好的"),
        ];
        let r = compiler.compile("t1", &msgs);
        assert!(r.session.current_goal.contains("prompt"));
    }

    #[test]
    fn compile_unanswered_is_open_problem() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "有个bug"),
            Message::new("assistant", "修好了"),
            Message::new("user", "性能太差"),
        ];
        let r = compiler.compile("t1", &msgs);
        assert!(r.session.open_problems.iter().any(|p| p.contains("性能")));
    }

    /// Objective: Verify ordinary emotion/preference messages now produce
    /// observations (the expanded marker set). Previously only 12 hardcoded
    /// words matched, so "我很焦虑，压力很大" compiled zero facts and the
    /// facts table stayed empty.
    /// Invariants: each emotion message yields an observation carrying the
    /// original content as evidence.
    #[test]
    fn compile_user_observations_covers_emotion_and_preference() {
        let msgs = vec![
            Message::new("user", "我很焦虑，压力很大。"),
            Message::new("user", "我欣赏曹操的知人善任。"),
            Message::new("user", "我打算下周发布新版本。"),
        ];
        let obs = compile_user_observations(&msgs, 1);
        assert!(
            obs.iter()
                .any(|o| o.action == "feel" && o.evidence.is_some()),
            "焦虑/压力 must yield a feel observation, got {obs:?}"
        );
        assert!(
            obs.iter()
                .any(|o| o.action == "喜欢" && o.evidence.is_some()),
            "欣赏 must yield a 喜欢 observation, got {obs:?}"
        );
        assert!(
            obs.iter().any(|o| o.action == "打算"),
            "打算 must yield an observation, got {obs:?}"
        );
    }

    #[test]
    fn compile_detects_decision_via_done() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "把lancedb换成sqlite-vec行不行？"),
            Message::new("assistant", "Done，已经替换了"),
        ];
        let r = compiler.compile("t1", &msgs);
        assert!(!r.decisions.is_empty());
        assert!(r.decisions[0].decision.contains("lancedb"));
    }

    #[test]
    fn compile_knowledge_deduped() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "为什么慢？"),
            Message::new("assistant", "因为lancedb太重"),
            Message::new("user", "为什么慢？"),
            Message::new("assistant", "因为lancedb太重"),
        ];
        let result = compiler.compile("t1", &msgs);
        assert!(
            result.knowledge.len() <= 1,
            "Equivalent knowledge should be emitted at most once"
        );
    }

    /// Objective: Verify conversation input crosses the unified Observation IR.
    /// Invariants: Assistant text is ignored and user goals retain source evidence.
    #[test]
    fn user_observations_only_compile_user_authored_cognition() {
        let messages = vec![
            Message::new("user", "我准备找一份 Rust 工作"),
            Message::new("assistant", "我喜欢 Go"),
        ];

        let observations = compile_user_observations(&messages, 42);

        assert_eq!(
            observations.len(),
            1,
            "Only the user's goal should become an observation"
        );
        assert_eq!(
            observations[0].subject.entity_id,
            Some(42),
            "Observation should target the configured User entity"
        );
        assert_eq!(
            observations[0].action, "准备",
            "The goal marker should retain its semantic action"
        );
        assert!(
            observations[0].evidence.is_some(),
            "Every conversation observation should retain source evidence"
        );
    }

    /// Objective: Verify user observations become typed immutable facts.
    /// Invariants: Goal and emotion facts share the User id and supplied logical time.
    #[test]
    fn user_facts_preserve_type_entity_and_time() {
        let messages = vec![Message::new("user", "我计划学 Rust，但最近压力很大")];

        let facts = compile_user_facts(&messages, 99, 20260730);

        assert_eq!(
            facts.len(),
            2,
            "Both goal and emotion signals should produce facts"
        );
        assert!(
            facts.iter().any(|fact| fact.fact_type == FactType::Goal),
            "Plan signal should compile to a Goal fact"
        );
        assert!(
            facts.iter().any(|fact| fact.fact_type == FactType::Emotion),
            "Stress signal should compile to an Emotion fact"
        );
        assert!(
            facts.iter().all(|fact| fact.entity_id == 99),
            "All facts should belong to the configured User entity"
        );
        assert!(
            facts.iter().all(|fact| fact.time == 20260730),
            "All facts should retain the supplied logical time"
        );
    }
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
            }
        })
        .collect()
}

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
