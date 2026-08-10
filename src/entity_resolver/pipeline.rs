//! Resolver Pipeline — chains multiple [`ResolverStage`]s together.
//!
//! Each stage is tried in order. The first stage that returns `Matched` wins;
//! if no stage produces a match, the result is `Unknown`.

use crate::entity_resolver::ResolveResult;

/// Context passed to each resolver stage.
///
/// V1 is minimal — stages receive the raw mention and return a result.
/// Future versions may add the current sentence, document title, etc.
pub struct ResolveContext {
    /// The raw mention text as it appeared in the original sentence.
    pub surface: String,
}

/// A single stage in the resolution pipeline.
///
/// Each stage is responsible for a specific resolution strategy (exact alias,
/// embedding similarity, regex, pronoun, etc.).
pub trait ResolverStage: Send + Sync {
    /// Attempt to resolve `mention` to an entity.
    ///
    /// Returns `Some(ResolveResult::Matched(...))` on success,
    /// `Some(ResolveResult::Unknown(...))` if the stage knows the mention is
    /// unresolvable (e.g. a stop-word filter), or `None` to pass to the next
    /// stage.
    fn resolve(&self, mention: &str, ctx: &ResolveContext) -> Option<ResolveResult>;
}

/// A chain of resolver stages executed in order.
pub struct ResolverPipeline {
    stages: Vec<Box<dyn ResolverStage>>,
}

impl ResolverPipeline {
    /// Create an empty pipeline with no stages.
    pub fn empty() -> Self {
        ResolverPipeline { stages: Vec::new() }
    }

    /// Create a pipeline with the given stages.
    pub fn new(stages: Vec<Box<dyn ResolverStage>>) -> Self {
        ResolverPipeline { stages }
    }

    /// Append a stage to the pipeline.
    pub fn push(&mut self, stage: impl ResolverStage + 'static) {
        self.stages.push(Box::new(stage));
    }

    /// Resolve `mention` by trying each stage in order.
    ///
    /// Returns the first `Matched` result, or `Unknown` if no stage matched.
    pub fn resolve(&self, mention: &str) -> ResolveResult {
        self.resolve_with_stage(mention).0
    }

    /// Resolve `mention`, reporting which stage produced the result.
    ///
    /// Returns `(result, Some(stage_index))` when a stage returned a result
    /// (the index identifies the winning stage, letting callers distinguish
    /// an alias hit from an embedding hit), or `(Unknown, None)` when no
    /// stage matched.
    pub fn resolve_with_stage(&self, mention: &str) -> (ResolveResult, Option<usize>) {
        let ctx = ResolveContext {
            surface: mention.to_string(),
        };

        for (idx, stage) in self.stages.iter().enumerate() {
            if let Some(result) = stage.resolve(mention, &ctx) {
                return (result, Some(idx));
            }
        }

        (
            ResolveResult::Unknown {
                surface: mention.to_string(),
            },
            None,
        )
    }

    /// Number of stages in the pipeline.
    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyMatcher {
        keyword: String,
        entity_id: i64,
    }

    impl ResolverStage for DummyMatcher {
        fn resolve(&self, mention: &str, _ctx: &ResolveContext) -> Option<ResolveResult> {
            if mention.contains(&self.keyword) {
                Some(ResolveResult::Matched {
                    entity_id: self.entity_id,
                    score: 1.0,
                })
            } else {
                None // pass to next stage
            }
        }
    }

    /// Objective: Verify that the pipeline returns the first stage's match.
    /// Invariants: First matching stage wins; later stages are not consulted.
    #[test]
    fn first_match_wins() {
        let mut pipeline = ResolverPipeline::empty();
        pipeline.push(DummyMatcher {
            keyword: "刘".into(),
            entity_id: 10001,
        });
        pipeline.push(DummyMatcher {
            keyword: "曹".into(),
            entity_id: 10002,
        });

        let result = pipeline.resolve("刘备");
        assert_eq!(
            result.entity_id(),
            Some(10001),
            "first stage should match '刘'"
        );
    }

    /// Objective: Verify that `Unknown` is returned when no stage matches.
    /// Invariants: Unrelated mention produces Unknown.
    #[test]
    fn no_match_returns_unknown() {
        let pipeline = ResolverPipeline::empty();
        let result = pipeline.resolve("朴素的");
        assert!(!result.is_matched(), "empty pipeline should return Unknown");
    }

    /// Objective: Verify pipeline supports dynamic stage addition.
    /// Invariants: After push, new matches work.
    #[test]
    fn dynamic_stage_addition() {
        let mut pipeline = ResolverPipeline::empty();
        pipeline.push(DummyMatcher {
            keyword: "曹操".into(),
            entity_id: 42,
        });
        let result = pipeline.resolve("曹操");
        assert_eq!(result.entity_id(), Some(42));
    }
}
