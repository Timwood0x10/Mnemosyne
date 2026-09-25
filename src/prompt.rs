use crate::types::{CompiledConversation, Message};

pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(&self, recent_messages: &[Message], compiled: &CompiledConversation) -> String {
        let mut parts: Vec<String> = Vec::new();

        // Knowledge: top 5 by importance.
        if !compiled.knowledge.is_empty() {
            let mut sorted = compiled.knowledge.clone();
            sorted.sort_by(|a, b| {
                b.importance
                    .partial_cmp(&a.importance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            parts.push("## Knowledge".to_string());
            for mem in sorted.iter().take(5) {
                let line = if mem.summary.is_empty() {
                    &mem.content
                } else {
                    &mem.summary
                };
                parts.push(format!(
                    "- [{}%] {}",
                    (mem.importance * 100.0).round() as u32,
                    line
                ));
            }
            parts.push(String::new());
        }

        // Decisions: top 3.
        if !compiled.decisions.is_empty() {
            let mut sorted = compiled.decisions.clone();
            sorted.sort_by(|a, b| {
                b.importance
                    .partial_cmp(&a.importance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            parts.push("## Decisions".to_string());
            for d in sorted.iter().take(3) {
                parts.push(format!("- {} ({})", d.decision, d.module));
            }
            parts.push(String::new());
        }

        // Session state.
        let s = &compiled.session;
        parts.push("## Session State".to_string());
        if !s.current_goal.is_empty() {
            parts.push(format!("- Goal: {}", s.current_goal));
        }
        if !s.current_module.is_empty() {
            parts.push(format!("- Module: {}", s.current_module));
        }
        if !s.current_files.is_empty() {
            parts.push(format!("- Files: {}", s.current_files.join(", ")));
        }
        if !s.open_problems.is_empty() {
            parts.push(format!("- Problems: {}", s.open_problems.join("; ")));
        }
        // Reasoning chain.
        if !s.reasoning_chain.is_empty() {
            parts.push("## Reasoning Chain".to_string());
            for step in &s.reasoning_chain {
                parts.push(format!("- Trigger: {}", step.trigger));
                parts.push(format!("  Tool: {}({})", step.tool_name, step.tool_args));
                parts.push(format!("  Status: {}", step.status));
                if !step.reasoning.is_empty() {
                    parts.push(format!("  → {}", step.reasoning));
                }
            }
            parts.push(String::new());
        }

        // Recent messages: last 3, trimmed.
        if !recent_messages.is_empty() {
            parts.push("## Recent".to_string());
            for msg in recent_messages.iter().rev().take(3).rev() {
                let c: String = msg.content.chars().take(200).collect();
                // Ellipsis decision must use the SAME unit as the truncation
                // (characters). The old `msg.content.len() > 200` compared
                // bytes: a Chinese message of 100 chars (300 bytes) got a
                // spurious "…" even though nothing was cut, and messages
                // between 200 and 600 bytes were truncated without any marker.
                let s = if msg.content.chars().count() > 200 {
                    "…"
                } else {
                    ""
                };
                parts.push(format!("{}: {}{}", msg.role, c, s));
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
    fn build_empty() {
        let p = PromptBuilder.build(&[], &CompiledConversation::default());
        assert!(p.contains("Session State"));
    }

    #[test]
    fn build_shows_knowledge() {
        let mut mem = Memory::new("t1", MemoryType::Knowledge, "full", 0.8);
        mem.summary = "用sqlite-vec替换lancedb".to_string();
        let c = CompiledConversation {
            knowledge: vec![mem],
            ..Default::default()
        };
        let p = PromptBuilder.build(&[], &c);
        assert!(p.contains("sqlite-vec"));
        assert!(p.contains("80%"));
    }

    #[test]
    fn build_shows_decision() {
        let c = CompiledConversation {
            decisions: vec![Decision {
                decision: "换用sqlite-vec".into(),
                rationale: "lancedb太重".into(),
                module: "store".into(),
                importance: 0.8,
            }],
            ..Default::default()
        };
        let p = PromptBuilder.build(&[], &c);
        assert!(p.contains("Decisions"));
        assert!(p.contains("sqlite-vec"));
    }

    #[test]
    fn build_includes_recent() {
        let msgs = vec![
            Message::new("user", "问题1"),
            Message::new("assistant", "回答1"),
        ];
        let p = PromptBuilder.build(&msgs, &CompiledConversation::default());
        assert!(p.contains("问题1"));
    }
}
