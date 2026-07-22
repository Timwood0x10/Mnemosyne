use crate::types::{CompiledConversation, Message};

pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(
        &self,
        recent_messages: &[Message],
        compiled: &CompiledConversation,
    ) -> String {
        let mut parts: Vec<String> = Vec::new();

        // Long-term memories.
        if !compiled.knowledge.is_empty() {
            parts.push("## Knowledge".to_string());
            for mem in &compiled.knowledge {
                let line = if mem.summary.is_empty() {
                    mem.content.clone()
                } else {
                    mem.summary.clone()
                };
                parts.push(format!("- {}", line));
            }
            parts.push(String::new());
        }

        // Decisions.
        if !compiled.decisions.is_empty() {
            parts.push("## Decisions".to_string());
            for d in &compiled.decisions {
                parts.push(format!("- {} ({})", d.decision, d.module));
            }
            parts.push(String::new());
        }

        // Session state.
        let session = &compiled.session;
        parts.push("## Session State".to_string());
        if !session.current_goal.is_empty() {
            parts.push(format!("- Goal: {}", session.current_goal));
        }
        if !session.current_module.is_empty() {
            parts.push(format!("- Module: {}", session.current_module));
        }
        if !session.current_files.is_empty() {
            parts.push(format!("- Files: {}", session.current_files.join(", ")));
        }
        if !session.open_problems.is_empty() {
            parts.push(format!("- Problems: {}", session.open_problems.join("; ")));
        }
        if !session.todo.is_empty() {
            parts.push(format!("- TODO: {}", session.todo.join("; ")));
        }
        if !session.recent_decisions.is_empty() {
            parts.push(format!(
                "- Recent decisions: {}",
                session.recent_decisions.join("; ")
            ));
        }
        parts.push(String::new());

        // Recent messages.
        if !recent_messages.is_empty() {
            parts.push("## Recent".to_string());
            for msg in recent_messages.iter().rev().take(3).rev() {
                parts.push(format!("{}: {}", msg.role, msg.content));
            }
        }

        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Decision, Memory, MemoryType};

    #[test]
    fn build_with_empty_compiled() {
        let builder = PromptBuilder;
        let prompt = builder.build(&[], &CompiledConversation::default());
        assert!(prompt.contains("Session State"), "should include session header");
        assert!(!prompt.contains("## Recent"), "no recent messages");
    }

    #[test]
    fn build_includes_knowledge() {
        let builder = PromptBuilder;
        let mut mem = Memory::new("t1", MemoryType::Knowledge, "full", 0.8);
        mem.summary = "用sqlite-vec替换lancedb".to_string();
        let compiled = CompiledConversation {
            knowledge: vec![mem],
            ..CompiledConversation::default()
        };
        let prompt = builder.build(&[], &compiled);
        assert!(prompt.contains("sqlite-vec"), "should include knowledge");
    }

    #[test]
    fn build_includes_decisions() {
        let builder = PromptBuilder;
        let compiled = CompiledConversation {
            decisions: vec![Decision {
                decision: "换用sqlite-vec".to_string(),
                rationale: "lancedb太重".to_string(),
                module: "store".to_string(),
                importance: 0.8,
            }],
            ..CompiledConversation::default()
        };
        let prompt = builder.build(&[], &compiled);
        assert!(prompt.contains("Decisions"), "should have decisions section");
        assert!(prompt.contains("sqlite-vec"), "should mention decision");
    }

    #[test]
    fn build_includes_recent_messages() {
        let builder = PromptBuilder;
        let msgs = vec![
            Message::new("user", "问题1"),
            Message::new("assistant", "回答1"),
            Message::new("user", "问题2"),
        ];
        let prompt = builder.build(&msgs, &CompiledConversation::default());
        assert!(prompt.contains("问题2"), "should include latest message");
    }
}
