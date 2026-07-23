use std::collections::HashSet;

use crate::classifier::MemoryClassifier;
use crate::extractor::{ExperienceExtractor, ExtractorConfig};
use crate::filter::NoiseFilter;
use crate::scorer::ImportanceScorer;
use crate::types::{
    CompiledConversation, Decision, Memory, MemoryType, Message, ReasoningStep, SessionState,
};

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

    pub fn compile(&self, messages: &[Message]) -> CompiledConversation {
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
                let mut mem = Memory::new("default", memory_type, &raw.solution, importance);
                mem.summary = if raw.problem.is_empty() {
                    raw.solution.clone()
                } else {
                    format!("{}：{}", raw.problem, raw.solution)
                };
                knowledge.push(mem);
            }

            // Decision detection: solution mentions an action was taken
            let sol_lower = raw.solution.to_lowercase();
            let is_decision = sol_lower.contains("done")
                || sol_lower.contains("implemented")
                || sol_lower.contains("replaced")
                || sol_lower.contains("已")
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
        let r = compiler.compile(&msgs);
        assert!(!r.knowledge.is_empty());
    }

    #[test]
    fn compile_tracks_files() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![Message::new("user", "修改compiler.rs 和 prompt.rs")];
        let r = compiler.compile(&msgs);
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
        let r = compiler.compile(&msgs);
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
        let r = compiler.compile(&msgs);
        assert!(r.session.open_problems.iter().any(|p| p.contains("性能")));
    }

    #[test]
    fn compile_detects_decision_via_done() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "把lancedb换成sqlite-vec行不行？"),
            Message::new("assistant", "Done，已经替换了"),
        ];
        let r = compiler.compile(&msgs);
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
        let r = compiler.compile(&msgs);
        assert!(r.knowledge.len() <= 1);
    }
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
        let result = compiler.compile(&msgs);
        assert!(result.session.reasoning_chain.is_empty());
    }
}
