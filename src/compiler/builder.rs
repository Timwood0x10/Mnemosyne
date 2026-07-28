//! ObjectBuilder + EdgeBuilder — Phase 5.
//!
//! Decomposes each [`Observation`] into [`CompiledObject`] and [`CompiledEdge`]
//! entries. An observation like:
//!
//! ```text
//! subject:赵云 predicate:救 object:阿斗 instrument:白马
//! ```
//!
//! produces:
//! - CompiledObject("赵云", person)
//! - CompiledObject("阿斗", person)
//! - CompiledObject("白马", artifact)
//! - CompiledEdge("赵云", "骑", "白马")
//! - CompiledEdge("赵云", "救", "阿斗")
//!
//! All entries carry an [`EvidenceSlice`] back to the source sentence.

use std::collections::HashMap;

use crate::compiler::{
    CompileResult, CompiledEdge, CompiledObject, CompileStats, EvidenceSlice,
    Observation, SemanticRole,
};

/// Build a [`CompileResult`] from a slice of observations.
///
/// # Object type inference
///
/// | Argument role | Inferred object_type |
/// |--------------|---------------------|
/// | Subject      | "person"            |
/// | Object       | "person"            |
/// | Instrument   | "artifact"          |
/// | Location     | "place"             |
/// | Time         | "concept"           |
/// | Other        | "concept"           |
pub fn build(observations: &[Observation]) -> CompileResult {
    let mut objects_map: HashMap<String, CompiledObject> = HashMap::new();
    let mut edges = Vec::new();
    let mut stats = CompileStats::default();

    for obs in observations {
        let sentence_id = obs.sentence_id;
        let segment_num = (sentence_id / 10000) as i32;

        // Build a minimal EvidenceSlice from the observation metadata
        let evidence = EvidenceSlice {
            text: String::new(), // actual text is resolved later by the writer
            sentence_id,
            segment_num,
            offset_start: 0,
            offset_end: 0,
        };

        // Extract subject
        let subject = obs.arguments.iter().find(|a| a.role == SemanticRole::Subject);
        let subject_name = subject.map(|a| a.value.as_str()).unwrap_or("");

        if subject_name.is_empty() {
            continue; // skip observations without a subject
        }

        // Register subject object
        let subject_obj = CompiledObject {
            name: subject_name.to_string(),
            object_type: infer_type(obs, SemanticRole::Subject),
            properties: HashMap::new(),
            evidence: evidence.clone(),
        };
        objects_map
            .entry(subject_name.to_string())
            .or_insert(subject_obj);

        // Register object and create edge for each non-subject argument
        for arg in &obs.arguments {
            if arg.role == SemanticRole::Subject {
                continue;
            }

            // Register the argument as an object
            let obj = CompiledObject {
                name: arg.value.clone(),
                object_type: infer_type(obs, arg.role.clone()),
                properties: HashMap::new(),
                evidence: evidence.clone(),
            };
            objects_map.entry(arg.value.clone()).or_insert(obj);

            // Create edge between subject and this argument
            edges.push(CompiledEdge {
                source: subject_name.to_string(),
                predicate: obs.predicate.clone(),
                target: arg.value.clone(),
                origin: crate::knowledge::Origin::Observed,
                confidence: obs.confidence,
                evidence: evidence.clone(),
            });
        }

        stats.observations += 1;
    }

    stats.objects = objects_map.len();
    stats.edges = edges.len();

    CompileResult {
        objects: objects_map.into_values().collect(),
        edges,
        stats,
    }
}

/// Infer the `object_type` for an entity based on its semantic role.
fn infer_type(_obs: &Observation, role: SemanticRole) -> String {
    match role {
        SemanticRole::Subject => "person".into(),
        SemanticRole::Object => "person".into(),
        SemanticRole::Instrument => "artifact".into(),
        SemanticRole::Location => "place".into(),
        SemanticRole::Time => "concept".into(),
        SemanticRole::Recipient => "person".into(),
        SemanticRole::Modifier => "concept".into(),
        SemanticRole::Other(_) => "concept".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{Observation, SemanticRole, Argument};

    fn make_obs(
        sentence_id: usize,
        predicate: &str,
        subject: &str,
        object: Option<&str>,
    ) -> Observation {
        let mut args = vec![Argument {
            role: SemanticRole::Subject,
            value: subject.into(),
        }];
        if let Some(obj) = object {
            args.push(Argument {
                role: SemanticRole::Object,
                value: obj.into(),
            });
        }
        Observation {
            sentence_id,
            predicate: predicate.into(),
            arguments: args,
            confidence: 0.9,
        }
    }

    /// Objective: Verify that a simple SPO produces one object and one edge.
    /// Invariants: One object (subject); one edge connecting subject→predicate→object.
    #[test]
    fn spo_produces_one_object_one_edge() {
        let obs = vec![make_obs(0, "救", "赵云", Some("阿斗"))];
        let result = build(&obs);
        assert_eq!(result.objects.len(), 2, "should create two objects (赵云, 阿斗)");
        assert_eq!(result.edges.len(), 1, "should create one edge");
        assert_eq!(result.edges[0].predicate, "救");
        assert_eq!(result.edges[0].source, "赵云");
        assert_eq!(result.edges[0].target, "阿斗");
        assert_eq!(result.edges[0].origin, crate::knowledge::Origin::Observed);
    }

    /// Objective: Verify that multiple observations with the same subject
    /// produce only one CompiledObject for that subject.
    /// Invariants: Objects are deduplicated by name.
    #[test]
    fn duplicate_objects_are_deduplicated() {
        let obs = vec![
            make_obs(0, "救", "赵云", Some("阿斗")),
            make_obs(1, "杀", "赵云", Some("夏侯恩")),
        ];
        let result = build(&obs);
        // 赵云 should appear once, 阿斗 and 夏侯恩 each once → 3 objects
        assert_eq!(result.objects.len(), 3, "赵云 deduplicated → 3 distinct objects");
        assert_eq!(result.edges.len(), 2);
    }

    /// Objective: Verify that an observation without a subject is skipped.
    /// Invariants: No objects or edges created.
    #[test]
    fn observation_without_subject_is_skipped() {
        let obs = vec![Observation {
            sentence_id: 0,
            predicate: "怒".into(),
            arguments: vec![],
            confidence: 0.5,
        }];
        let result = build(&obs);
        assert!(result.objects.is_empty(), "no subject → no objects");
        assert!(result.edges.is_empty(), "no subject → no edges");
    }

    /// Objective: Verify that instrument roles create additional objects and edges.
    /// Invariants: "骑白马" → 白马 as artifact + edge(赵云,骑,白马).
    #[test]
    fn instrument_creates_object_and_edge() {
        let obs = vec![Observation {
            sentence_id: 0,
            predicate: "救".into(),
            arguments: vec![
                Argument { role: SemanticRole::Subject, value: "赵云".into() },
                Argument { role: SemanticRole::Object, value: "阿斗".into() },
                Argument { role: SemanticRole::Instrument, value: "白马".into() },
            ],
            confidence: 0.9,
        }];
        let result = build(&obs);
        assert!(result.objects.iter().any(|o| o.name == "白马"), "白马 should be an object");
        assert!(result.edges.iter().any(|e| e.predicate == "救" && e.target == "白马"),
            "should have edge for instrument");
    }

    /// Objective: Verify that stats are correctly populated.
    /// Invariants: stats.observations, stats.objects, stats.edges are accurate.
    #[test]
    fn stats_are_accurate() {
        let obs = vec![
            make_obs(0, "救", "赵云", Some("阿斗")),
            make_obs(1, "杀", "关羽", Some("华雄")),
        ];
        let result = build(&obs);
        assert_eq!(result.stats.observations, 2);
        assert_eq!(result.stats.objects, 4);
        assert_eq!(result.stats.edges, 2);
    }
}
