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

mod markers;

use markers::{FUNCTIONAL_MATCHER, OBSERVATION_MARKERS};

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
    // `config/markers_en.json` (loaded once via OBSERVATION_MARKERS, editable
    // without recompiling), falling back to the built-in default table when
    // the config is missing. See OBSERVATION_MARKERS for the multi-character
    // false-positive notes.
    let actions = OBSERVATION_MARKERS.as_slice();

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
                if negation_near(&message.content, offset, marker.len(), &cues) {
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
pub fn compile_user_facts(messages: &[Message], user_entity_id: i64, time: i32) -> Vec<Fact> {
    let observations = compile_user_observations(messages, user_entity_id);
    user_facts_from_observations(&observations, time)
}

#[cfg(test)]
mod tests {
    use super::markers::merge_marker_files;
    use super::*;

    /// Objective: Verify the two shipped marker files merge — Chinese words
    /// (幸福 → feel) and English words (excited → feel, adore → love) both
    /// contribute; users can customize each language's file independently.
    /// Invariants: merged pairs contain zh and en markers with their actions.
    #[test]
    fn marker_files_merge_zh_and_en() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zh = dir.path().join("markers_zh.json");
        let en = dir.path().join("markers_en.json");
        std::fs::write(&zh, r#"{"feel": ["幸福", "失眠"], "plan": ["目标"]}"#).expect("write zh");
        std::fs::write(
            &en,
            r#"{"feel": ["excited", "burned out"], "love": ["adore"]}"#,
        )
        .expect("write en");

        let pairs = merge_marker_files(&[zh, en]);
        assert!(
            pairs.contains(&("幸福".to_string(), "feel".to_string())),
            "zh feel marker merged, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("目标".to_string(), "plan".to_string())),
            "zh plan marker merged, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("excited".to_string(), "feel".to_string())),
            "en feel marker merged, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("adore".to_string(), "love".to_string())),
            "en love marker merged, got {pairs:?}"
        );
    }

    /// Objective: Verify the fail-soft fallback — when BOTH marker files are
    /// missing/corrupt, the built-in default table is used so a config-less
    /// deployment keeps compiling facts.
    /// Invariants: merge of two nonexistent paths yields the default table
    /// (non-empty, includes the core 喜欢 and feel markers).
    #[test]
    fn marker_files_fallback_to_defaults_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist.json");
        let pairs = merge_marker_files(&[missing.clone(), missing]);
        assert!(
            !pairs.is_empty(),
            "fallback must be non-empty, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("喜欢".to_string(), "喜欢".to_string())),
            "default 喜欢 marker present, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("疲惫".to_string(), "feel".to_string())),
            "default feel marker present, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("love".to_string(), "love".to_string())),
            "default English marker present, got {pairs:?}"
        );
    }

    /// Objective: Verify one valid + one missing file still yields the valid
    /// file's markers (partial load is not an all-or-nothing failure).
    /// Invariants: only the valid zh file's markers appear.
    #[test]
    fn marker_files_partial_load_keeps_valid_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zh = dir.path().join("markers_zh.json");
        std::fs::write(&zh, r#"{"want": ["想学"]}"#).expect("write zh");
        let missing = dir.path().join("markers_en.json");

        let pairs = merge_marker_files(&[zh, missing]);
        assert!(
            pairs.contains(&("想学".to_string(), "want".to_string())),
            "valid file markers kept, got {pairs:?}"
        );
        assert_eq!(pairs.len(), 1, "no extra markers from the missing file");
    }

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
            obs.iter().any(|o| o.action == "plan"),
            "打算 must yield a plan observation, got {obs:?}"
        );
    }

    /// Objective: Verify MODERN vernacular speech — words the classic novel
    /// lexicon does not carry (加班/应酬/失眠/疲惫/孤独) — now yields feel
    /// observations. Daily-life conversations previously compiled zero facts.
    /// Invariants: a message about overwork/insomnia produces a feel
    /// observation; a loneliness message produces one too.
    #[test]
    fn modern_vernacular_speech_produces_observations() {
        let msgs = vec![
            Message::new("user", "最近天天加班，应酬也多，晚上还失眠。"),
            Message::new("user", "一个人在外地，有时候挺孤独的。"),
        ];
        let obs = compile_user_observations(&msgs, 1);
        assert!(
            obs.iter()
                .any(|o| o.action == "feel" && o.evidence.is_some()),
            "加班/应酬/失眠 must yield a feel observation, got {obs:?}"
        );
        let evidence_texts: Vec<&str> = obs
            .iter()
            .filter_map(|o| o.evidence.as_ref().map(|e| e.text.as_str()))
            .collect();
        assert!(
            evidence_texts.iter().any(|t| t.contains("加班")),
            "evidence must reference the 加班 message, got {evidence_texts:?}"
        );
        assert!(
            evidence_texts.iter().any(|t| t.contains("孤独")),
            "孤独 message must also produce evidence, got {evidence_texts:?}"
        );
    }

    /// Objective: Verify the broadened high-frequency Chinese vocabulary —
    /// positive emotions (幸福/高兴/满足), negative emotions (烦恼/绝望/
    /// 崩溃), plans (目标/梦想), and wants (想学/想去) — all produce
    /// observations with evidence, and that multi-char markers prevent
    /// single-char false positives ("想" alone would match 想象/想法).
    /// Invariants: each category message yields its mapped action.
    #[test]
    fn high_frequency_chinese_vocabulary_produces_observations() {
        let msgs = vec![
            Message::new("user", "今天很幸福，也很高兴能和你聊天。"),
            Message::new("user", "最近特别烦恼，感觉快崩溃了。"),
            Message::new("user", "我的目标是明年发布自己的产品，这是我的梦想。"),
            Message::new("user", "我想学钢琴，想去欧洲旅行。"),
        ];
        let obs = compile_user_observations(&msgs, 1);
        let evidence_texts: Vec<&str> = obs
            .iter()
            .filter_map(|o| o.evidence.as_ref().map(|e| e.text.as_str()))
            .collect();
        assert!(
            evidence_texts.iter().any(|t| t.contains("幸福")),
            "幸福 must yield evidence, got {evidence_texts:?}"
        );
        assert!(
            evidence_texts.iter().any(|t| t.contains("崩溃")),
            "崩溃 must yield evidence, got {evidence_texts:?}"
        );
        assert!(
            obs.iter()
                .any(|o| o.action == "plan" && o.evidence.is_some()),
            "目标/梦想 must yield a plan observation, got {obs:?}"
        );
        assert!(
            obs.iter()
                .any(|o| o.action == "want" && o.evidence.is_some()),
            "想学/想去 must yield a want observation, got {obs:?}"
        );
        // Single-char 想 must not fire on 想象: "想" alone is not a marker.
        let imagination = Message::new("user", "我的想象里有一片海。");
        let obs2 = compile_user_observations(std::slice::from_ref(&imagination), 1);
        assert!(
            obs2.is_empty(),
            "想象 must NOT match a bare 想 marker, got {obs2:?}"
        );
    }

    /// Objective: Verify duplicate-fact inflation is fixed — one message
    /// matching several SAME-action markers must yield a single observation
    /// for that action (失眠+加班+压力+好累 all → feel, so ONE feel obs, not
    /// four near-identical ones), while DIFFERENT actions are kept.
    /// Invariants: exactly one feel observation for the four-marker sentence;
    /// a mixed sentence yields one obs per distinct action.
    #[test]
    fn same_action_markers_dedup_to_single_observation() {
        // All four markers map to `feel`: previously 4 observations.
        let msgs = vec![Message::new("user", "最近天天失眠，还加班，压力好大，好累")];
        let obs = compile_user_observations(&msgs, 1);
        let feel_count = obs.iter().filter(|o| o.action == "feel").count();
        assert_eq!(
            feel_count, 1,
            "four feel markers in one sentence must yield ONE feel observation, got {obs:?}"
        );
        // Different actions in one sentence are all kept.
        let mixed = vec![Message::new("user", "我打算学钢琴，也很开心")];
        let obs2 = compile_user_observations(&mixed, 1);
        let actions: Vec<&str> = obs2.iter().map(|o| o.action.as_str()).collect();
        assert!(
            actions.contains(&"plan"),
            "plan marker kept alongside feel, got {actions:?}"
        );
        assert!(
            actions.contains(&"feel"),
            "feel marker kept alongside plan, got {actions:?}"
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

    /// Objective: Verify a negated preference is recorded as a negated state
    /// instead of being dropped. Discarding it made a user's negative stance
    /// unrepresentable and left `StanceFlip` unreachable for a user entity
    /// (negated facts existed only for the agent's own persona).
    /// Invariants: "我不喜欢应酬" keeps its `喜欢` (Preference) fact and its
    /// `应酬` (Emotion) fact, each tagged `negated: true` with the verbatim
    /// message preserved as content.
    #[test]
    fn negated_preference_is_recorded_as_a_negated_state() {
        let messages = vec![Message::new("user", "我不喜欢应酬")];

        let facts = compile_user_facts(&messages, 99, 20260730);

        let preference = facts
            .iter()
            .find(|fact| fact.fact_type == FactType::Preference)
            .expect("the 喜欢 marker must still compile to a Preference fact");
        assert_eq!(
            preference.payload["negated"],
            serde_json::Value::Bool(true),
            "the negation must be recorded on the fact"
        );
        assert_eq!(
            preference.payload["content"], "我不喜欢应酬",
            "the verbatim message is preserved"
        );
        assert!(
            facts
                .iter()
                .all(|fact| fact.payload["negated"] == serde_json::Value::Bool(true)),
            "every fact from a negated message carries the negation, got {facts:?}"
        );
    }

    /// Objective: Verify a negated plan never surfaces as an AFFIRMATIVE goal
    /// (ELITE_LEXICON_PLAN §13.3) while still being remembered as the negated
    /// state it is. Discarding the fact kept the rule but lost the information:
    /// "我不打算考公务员了" is exactly the kind of change a companion must not miss.
    /// Invariants: a Goal fact is produced, and it carries `negated: true`;
    /// exactly one Goal fact exists (no mirrored affirmative twin).
    #[test]
    fn negated_plan_is_kept_as_a_negated_goal() {
        let messages = vec![Message::new("user", "我不打算学 Rust")];

        let facts = compile_user_facts(&messages, 99, 20260730);
        let goals: Vec<&Fact> = facts
            .iter()
            .filter(|fact| fact.fact_type == FactType::Goal)
            .collect();

        assert_eq!(
            goals.len(),
            1,
            "the plan must be remembered once: {facts:?}"
        );
        assert_eq!(
            goals[0].payload["negated"],
            serde_json::Value::Bool(true),
            "the goal must be stored as negated, so the current-state projection \
             never lists it as an active plan"
        );
        assert!(
            !facts.iter().any(|fact| fact.fact_type == FactType::Goal
                && fact.payload["negated"] != serde_json::Value::Bool(true)),
            "no affirmative goal may be produced from a negated plan, got {facts:?}"
        );
    }

    /// Objective: Verify negation is resolved per marker in its own clause, not
    /// per message. A message-wide flag turned any sentence containing 不/没/别
    /// into a negated one: "想学吉他很久了，一直在纠结买不买" lost its plan
    /// entirely, because a cue in a later clause ("买不买") marked the whole
    /// message negated and negated plans were dropped.
    /// Invariants: the plan survives and is affirmative; the `买不买` cue does not
    /// touch it.
    #[test]
    fn negation_is_resolved_per_clause() {
        let messages = vec![Message::new("user", "想学吉他很久了，一直在纠结买不买")];

        let facts = compile_user_facts(&messages, 99, 20260730);
        let goal = facts
            .iter()
            .find(|fact| fact.fact_type == FactType::Goal)
            .unwrap_or_else(|| panic!("the plan must survive a cue in another clause: {facts:?}"));

        assert_eq!(
            goal.payload["negated"],
            serde_json::Value::Bool(false),
            "a cue in a different clause must not negate this plan: {facts:?}"
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
                ..Fact::default()
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
