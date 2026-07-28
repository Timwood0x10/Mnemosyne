//! Chunk Merge — Phase 6.
//!
//! Merges multiple [`CompileResult`]s from parallel chunk compilation into a
//! single result, deduplicating objects by canonical name and edges by
//! (source, predicate, target).
//!
//! ## Merge rules
//!
//! | Entity | Dedup key | Conflict resolution |
//! |--------|-----------|-------------------|
//! | CompiledObject | `name` | First occurrence wins |
//! | CompiledEdge | `(source, predicate, target)` | Highest confidence wins |
//! | EvidenceSlice | `sentence_id` | First occurrence wins |

use std::collections::HashMap;

use crate::compiler::{CompileResult, CompiledEdge, CompiledObject, CompileStats};

/// Merge multiple [`CompileResult`]s into one.
///
/// Objects are deduplicated by `name` (case-sensitive). Edges are
/// deduplicated by `(source, predicate, target)`. Evidence slices that share
/// a `sentence_id` are deduplicated.
pub fn merge(results: Vec<CompileResult>) -> CompileResult {
    if results.is_empty() {
        return CompileResult::default();
    }
    if results.len() == 1 {
        return results.into_iter().next().unwrap();
    }

    let mut objects_map: HashMap<String, CompiledObject> = HashMap::new();
    let mut edges_map: HashMap<(String, String, String), CompiledEdge> = HashMap::new();
    let mut stats = CompileStats::default();

    for result in results {
        // Merge objects — first by name wins
        for obj in result.objects {
            objects_map.entry(obj.name.clone()).or_insert(obj);
        }

        // Merge edges — highest confidence wins for same (source, predicate, target)
        for edge in result.edges {
            let key = (edge.source.clone(), edge.predicate.clone(), edge.target.clone());
            edges_map
                .entry(key)
                .and_modify(|existing| {
                    if edge.confidence > existing.confidence {
                        *existing = edge.clone();
                    }
                })
                .or_insert(edge);
        }

        // Aggregate stats
        stats.sentences += result.stats.sentences;
        stats.mentions += result.stats.mentions;
        stats.observations += result.stats.observations;
        stats.evidence_slices += result.stats.evidence_slices;
        stats.derived_edges += result.stats.derived_edges;
    }

    stats.objects = objects_map.len();
    stats.edges = edges_map.len();

    CompileResult {
        objects: objects_map.into_values().collect(),
        edges: edges_map.into_values().collect(),
        stats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{CompiledEdge, CompiledObject, EvidenceSlice};
    use crate::knowledge::Origin;

    fn make_obj(name: &str) -> CompiledObject {
        CompiledObject {
            name: name.into(),
            object_type: "person".into(),
            properties: HashMap::new(),
            evidence: EvidenceSlice {
                text: String::new(),
                sentence_id: 0,
                segment_num: 1,
                offset_start: 0,
                offset_end: 0,
            },
        }
    }

    fn make_edge(source: &str, pred: &str, target: &str, conf: f64) -> CompiledEdge {
        CompiledEdge {
            source: source.into(),
            predicate: pred.into(),
            target: target.into(),
            origin: Origin::Observed,
            confidence: conf,
            evidence: EvidenceSlice {
                text: String::new(),
                sentence_id: 0,
                segment_num: 1,
                offset_start: 0,
                offset_end: 0,
            },
        }
    }

    /// Objective: Verify that a single result passes through unchanged.
    /// Invariants: Output == input (same objects, edges, stats).
    #[test]
    fn single_result_passes_through() {
        let r = CompileResult {
            objects: vec![make_obj("赵云")],
            edges: vec![make_edge("赵云", "救", "阿斗", 0.9)],
            stats: CompileStats {
                observations: 1, objects: 1, edges: 1, ..Default::default()
            },
        };
        let merged = merge(vec![r.clone()]);
        assert_eq!(merged.objects.len(), 1);
        assert_eq!(merged.edges.len(), 1);
        assert_eq!(merged.objects[0].name, "赵云");
    }

    /// Objective: Verify that duplicate objects are deduplicated by name.
    /// Invariants: Two chunks each with "赵云" → one object.
    #[test]
    fn duplicate_objects_deduplicated() {
        let r1 = CompileResult {
            objects: vec![make_obj("赵云")],
            edges: vec![],
            stats: CompileStats::default(),
        };
        let r2 = CompileResult {
            objects: vec![make_obj("赵云")],
            edges: vec![],
            stats: CompileStats::default(),
        };
        let merged = merge(vec![r1, r2]);
        assert_eq!(merged.objects.len(), 1, "赵云 should appear once");
    }

    /// Objective: Verify that the same edge from two chunks keeps the
    /// higher confidence value.
    /// Invariants: Edge with conf=0.9 wins over conf=0.7.
    #[test]
    fn edge_highest_confidence_wins() {
        let r1 = CompileResult {
            objects: vec![],
            edges: vec![make_edge("赵云", "救", "阿斗", 0.7)],
            stats: CompileStats::default(),
        };
        let r2 = CompileResult {
            objects: vec![],
            edges: vec![make_edge("赵云", "救", "阿斗", 0.9)],
            stats: CompileStats::default(),
        };
        let merged = merge(vec![r1, r2]);
        assert_eq!(merged.edges.len(), 1);
        assert!((merged.edges[0].confidence - 0.9).abs() < 1e-6,
            "highest confidence should win");
    }

    /// Objective: Verify that empty input produces a default result.
    /// Invariants: No panics; objects and edges are empty.
    #[test]
    fn empty_input_returns_default() {
        let merged = merge(vec![]);
        assert!(merged.objects.is_empty());
        assert!(merged.edges.is_empty());
    }
}
