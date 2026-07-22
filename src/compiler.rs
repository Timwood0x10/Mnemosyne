use std::collections::HashSet;

use crate::classifier::MemoryClassifier;
use crate::extractor::{ExperienceExtractor, ExtractorConfig, RawExperience};
use crate::filter::NoiseFilter;
use crate::scorer::ImportanceScorer;
use crate::types::{
    CompiledConversation, Decision, Memory, MemoryType, Message, SessionState,
};

static MODULE_NAMES: &[&str] = &[
    "compiler", "distiller", "store", "detector", "prompt", "classifier",
    "scorer", "extractor", "filter", "resolver", "embed", "retrieval",
    "mcp", "types", "config", "error",
];
static FILE_PATTERNS: &[&str] = &[
    ".rs", ".toml", ".md", ".json", ".yaml", ".lock",
];
static TODO_PHRASES: &[&str] = &[
    "还要", "还需要", "接下来", "下一步", "待办", "剩下的", "未完成",
    "后续", "还有", "todo", "TODO", "to do", "下一步要",
];
static PROBLEM_PHRASES: &[&str] = &[
    "报错", "失败", "不工作", "坏了", "有问题", "不对", "不行",
    "错误", "bug", "issue", "问题", "怎么修", "怎么改",
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
            extractor: ExperienceExtractor::new(ExtractorConfig { enable_cross_turn: true }),
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
        let mut seen_modules = HashSet::new();
        let mut resolved_problems: HashSet<String> = HashSet::new();
        let mut unresolved_problems: HashSet<String> = HashSet::new();

        for window in messages.windows(2) {
            let (a, b) = (&window[0], &window[1]);
            if a.is_user() && b.is_assistant() {
                resolved_problems.insert(a.content.clone());
            }
        }

        for msg in messages {
            let c = &msg.content;

            if msg.is_user() {
                self.extract_goal(c, &mut session);
                self.extract_files(c, &mut seen_files, &mut session);
                self.extract_modules(c, &mut seen_modules, &mut session);
                self.extract_todo(c, &mut session);

                if !resolved_problems.contains(c)
                    && PROBLEM_PHRASES.iter().any(|p| c.contains(p))
                {
                    unresolved_problems.insert(c.clone());
                }
            }
        }

        session.open_problems = unresolved_problems.into_iter().collect();

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

            self.extract_decisions(raw, &mut decisions);
        }

        decisions.sort_by(|a, b| b.importance.partial_cmp(&a.importance).unwrap_or(std::cmp::Ordering::Equal));
        session.recent_decisions = decisions.iter().take(5).map(|d| d.decision.clone()).collect();

        CompiledConversation {
            knowledge,
            decisions,
            session,
        }
    }

    fn extract_goal(&self, c: &str, state: &mut SessionState) {
        let prefixes = [
            "现在目标是", "当前目标是", "现在要", "接下来要", "接下来需要",
            "需要实现", "帮我写", "帮我实现", "我的目标是", "我想",
            "我要", "现在需要", "帮我",
        ];
        for p in &prefixes {
            if let Some(rest) = c.strip_prefix(p) {
                let goal = rest.trim().trim_end_matches('？').trim_end_matches('?');
                if !goal.is_empty() {
                    state.current_goal = goal.to_string();
                    return;
                }
            }
        }

        let markers = ["把", "改为", "替换成", "加上", "实现", "写一个", "创建", "改成", "加一个"];
        for m in &markers {
            if let Some(pos) = c.find(m) {
                let goal = &c[pos..];
                let goal = goal.trim_end_matches('？').trim_end_matches('?');
                state.current_goal = goal.to_string();
                return;
            }
        }
    }

    fn extract_files(&self, c: &str, seen: &mut HashSet<String>, state: &mut SessionState) {
        for word in c.split(|c: char| c.is_whitespace() || c == '，' || c == '。' || c == '、' || c == '？' || c == '?' || c == '）' || c == '(' || c == ')') {
            let cleaned: String = word.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-').collect();
            if cleaned.len() >= 4 && FILE_PATTERNS.iter().any(|ext| cleaned.ends_with(ext)) && !seen.contains(&cleaned) {
                seen.insert(cleaned.clone());
                state.current_files.push(cleaned);
            }
        }
    }

    fn extract_modules(&self, c: &str, seen: &mut HashSet<String>, state: &mut SessionState) {
        for m in MODULE_NAMES {
            if c.contains(m) && !seen.contains(*m) {
                seen.insert(m.to_string());
                state.current_module = m.to_string();
            }
        }
    }

    fn extract_todo(&self, c: &str, state: &mut SessionState) {
        if TODO_PHRASES.iter().any(|p| c.contains(p)) {
            state.todo.push(c.to_string());
        }
    }

    fn extract_decisions(&self, raw: &RawExperience, decisions: &mut Vec<Decision>) {
        let decision_markers = [
            "换成", "改用", "替换", "改用", "决定", "decided",
            "replace", "switch", "改为", "改成",
        ];
        let is_decision = decision_markers
            .iter()
            .any(|m| raw.problem.contains(m) || raw.solution.contains(m));

        if is_decision {
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
                importance: 0.8,
            });
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
            Message::new("user", "为什么编译那么慢？怎么换成sqlite-vec？"),
            Message::new("assistant", "因为lancedb的polars feature太重了，换成sqlite-vec即可"),
        ];
        let result = compiler.compile(&msgs);
        assert!(!result.knowledge.is_empty());
        assert!(result.knowledge[0].summary.contains("sqlite-vec"));
    }

    #[test]
    fn compile_extracts_decisions() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "把lancedb换成sqlite-vec行不行？"),
            Message::new("assistant", "done，已替换"),
        ];
        let result = compiler.compile(&msgs);
        assert!(!result.decisions.is_empty());
    }

    #[test]
    fn compile_tracks_session_state() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "现在目标是实现compiler模块，在compiler.rs里"),
        ];
        let result = compiler.compile(&msgs);
        assert!(result.session.current_goal.contains("compiler"));
        assert!(result.session.current_files.contains(&"compiler.rs".to_string()));
    }

    #[test]
    fn compile_detects_open_problems() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "为什么编译那么慢？怎么换成sqlite-vec？"),
            Message::new("assistant", "因为lancedb太重了，换成sqlite-vec即可"),
            Message::new("user", "compiler模块还有bug，不工作"),
        ];
        let result = compiler.compile(&msgs);
        assert!(!result.session.open_problems.is_empty(), "should detect open problems");
        assert!(result.session.open_problems[0].contains("bug"), "problem mentions bug");
    }

    #[test]
    fn compile_detects_todo() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "接下来还要把prompt模块写完"),
        ];
        let result = compiler.compile(&msgs);
        assert!(!result.session.todo.is_empty(), "should detect todo");
        assert!(result.session.todo[0].contains("prompt"));
    }

    #[test]
    fn compile_tracks_multiple_files() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "修改compiler.rs 和 prompt.rs"),
        ];
        let result = compiler.compile(&msgs);
        assert!(result.session.current_files.contains(&"compiler.rs".to_string()));
        assert!(result.session.current_files.contains(&"prompt.rs".to_string()));
    }

    #[test]
    fn compile_resolved_problems_not_open() {
        let compiler = ConversationCompiler::new();
        let msgs = vec![
            Message::new("user", "100行有个bug，怎么修？"),
            Message::new("assistant", "改成42就行了"),
            Message::new("user", "还有个新问题：性能太差"),
        ];
        let result = compiler.compile(&msgs);
        // "有bug" was resolved (assistant replied), "性能太差" is still open (no assistant reply yet if it's the last msg)
        assert!(
            result.session.open_problems.iter().any(|p| p.contains("性能")),
            "unanswered problem should be open"
        );
    }
}
