//! Rule Engine — Phase 7.
//!
//! Applies configurable Edge→Edge patterns to a [`CompileResult`] and produces
//! derived edges. The engine is pure-memory — it reads from and writes to
//! [`CompileResult`], never touching SQLite directly.
//!
//! ## Rule format
//!
//! ```json
//! {
//!   "rules": [
//!     {
//!       "name": "rescue_implies_trust",
//!       "antecedent": { "predicate": "救" },
//!       "consequent": {
//!         "predicate": "信任",
//!         "confidence": 0.7
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! An antecedent matches any [`CompiledEdge`] whose `predicate` equals the
//! specified value. For each match, a derived edge is created with the
//! same source/target and the consequent's predicate + confidence.

use std::path::Path;

use serde::Deserialize;

use crate::compiler::{CompileResult, CompiledEdge};
use crate::knowledge::Origin;

/// A single rule definition.
#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub name: String,
    pub antecedent: Antecedent,
    pub consequent: Consequent,
}

/// The condition that must be matched.
#[derive(Debug, Clone, Deserialize)]
pub struct Antecedent {
    pub predicate: String,
}

/// The derived edge to create when the antecedent matches.
#[derive(Debug, Clone, Deserialize)]
pub struct Consequent {
    pub predicate: String,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

fn default_confidence() -> f64 {
    0.6
}

/// Rule engine configuration — a list of rules.
#[derive(Debug, Clone, Deserialize)]
pub struct RuleConfig {
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl Default for RuleConfig {
    fn default() -> Self {
        RuleConfig {
            rules: vec![
                Rule {
                    name: "rescue_implies_trust".into(),
                    antecedent: Antecedent { predicate: "救".into() },
                    consequent: Consequent {
                        predicate: "信任".into(),
                        confidence: 0.7,
                    },
                },
                Rule {
                    name: "kill_implies_hostility".into(),
                    antecedent: Antecedent { predicate: "杀".into() },
                    consequent: Consequent {
                        predicate: "敌对".into(),
                        confidence: 0.8,
                    },
                },
                Rule {
                    name: "serve_implies_aligned".into(),
                    antecedent: Antecedent { predicate: "属于".into() },
                    consequent: Consequent {
                        predicate: "同阵营".into(),
                        confidence: 0.6,
                    },
                },
            ],
        }
    }
}

/// The rule engine.
pub struct RuleEngine {
    rules: Vec<Rule>,
}

impl RuleEngine {
    /// Create a new engine with the default built-in rules.
    pub fn new() -> Self {
        RuleEngine {
            rules: RuleConfig::default().rules,
        }
    }

    /// Create an engine from a custom rule configuration.
    pub fn from_config(config: RuleConfig) -> Self {
        RuleEngine { rules: config.rules }
    }

    /// Load rules from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be read, or a parse error if
    /// the JSON is malformed.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: RuleConfig = serde_json::from_str(&content)?;
        Ok(RuleEngine::from_config(config))
    }

    /// Apply rules to a [`CompileResult`], producing derived edges.
    ///
    /// The original edges are preserved; derived edges are appended.
    /// No deduplication is performed here — that's the Merge phase's job.
    ///
    /// Returns a new [`CompileResult`] with derived edges added.
    pub fn apply(&self, input: &CompileResult) -> CompileResult {
        let mut derived = Vec::new();

        for edge in &input.edges {
            for rule in &self.rules {
                if edge.predicate == rule.antecedent.predicate {
                    let derived_edge = CompiledEdge {
                        source: edge.source.clone(),
                        predicate: rule.consequent.predicate.clone(),
                        target: edge.target.clone(),
                        origin: Origin::Derived,
                        confidence: rule.consequent.confidence * edge.confidence,
                        evidence: edge.evidence.clone(),
                    };
                    derived.push(derived_edge);
                }
            }
        }

        let mut stats = input.stats.clone();
        stats.derived_edges = derived.len();
        stats.edges = input.edges.len() + derived.len();

        let mut all_edges = input.edges.clone();
        all_edges.extend(derived);

        CompileResult {
            objects: input.objects.clone(),
            edges: all_edges,
            stats,
        }
    }
}

impl Default for RuleEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::CompileStats;
    use crate::compiler::{CompileResult, CompiledEdge, EvidenceSlice};
    use crate::knowledge::Origin;

    fn make_edge(source: &str, pred: &str, target: &str) -> CompiledEdge {
        CompiledEdge {
            source: source.into(),
            predicate: pred.into(),
            target: target.into(),
            origin: Origin::Observed,
            confidence: 0.9,
            evidence: EvidenceSlice {
                text: String::new(),
                sentence_id: 0,
                segment_num: 1,
                offset_start: 0,
                offset_end: 0,
            },
        }
    }

    /// Objective: Verify that a matching rule produces a derived edge.
    /// Invariants: Input edge count N, output has N+M derived edges.
    #[test]
    fn matching_rule_produces_derived_edge() {
        let engine = RuleEngine::new();
        let input = CompileResult {
            objects: vec![],
            edges: vec![make_edge("赵云", "救", "阿斗")],
            stats: CompileStats::default(),
        };
        let result = engine.apply(&input);
        // Original edge + derived "信任" edge
        assert_eq!(result.edges.len(), 2, "should have original + derived");
        let derived = result.edges.iter().find(|e| e.origin == Origin::Derived);
        assert!(derived.is_some(), "should have a derived edge");
        assert_eq!(derived.unwrap().predicate, "信任");
        assert_eq!(derived.unwrap().source, "赵云");
        assert_eq!(derived.unwrap().target, "阿斗");
    }

    /// Objective: Verify that non-matching rules produce no derived edges.
    /// Invariants: Output edges == input edges.
    #[test]
    fn non_matching_rule_skips() {
        let engine = RuleEngine::new();
        let input = CompileResult {
            objects: vec![],
            edges: vec![make_edge("赵云", "走", "荆州")], // "走" doesn't match any default rule
            stats: CompileStats::default(),
        };
        let result = engine.apply(&input);
        assert_eq!(result.edges.len(), 1, "no match → no derived edges");
        assert_eq!(result.edges[0].origin, Origin::Observed);
    }

    /// Objective: Verify that derived edges have their origin set correctly.
    /// Invariants: origin == Origin::Derived for all derived edges.
    #[test]
    fn derived_edge_origin_is_correct() {
        let engine = RuleEngine::new();
        let input = CompileResult {
            objects: vec![],
            edges: vec![make_edge("关羽", "杀", "华雄")],
            stats: CompileStats::default(),
        };
        let result = engine.apply(&input);
        let derived: Vec<&CompiledEdge> = result.edges.iter().filter(|e| e.origin == Origin::Derived).collect();
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].predicate, "敌对");
    }

    /// Objective: Verify that stats are updated correctly after rule application.
    /// Invariants: derived_edges count matches the number of derived edges.
    #[test]
    fn stats_reflect_derived_edges() {
        let engine = RuleEngine::new();
        let input = CompileResult {
            objects: vec![],
            edges: vec![make_edge("赵云", "救", "阿斗")],
            stats: CompileStats::default(),
        };
        let result = engine.apply(&input);
        assert_eq!(result.stats.derived_edges, 1);
        assert_eq!(result.stats.edges, 2);
    }
}
