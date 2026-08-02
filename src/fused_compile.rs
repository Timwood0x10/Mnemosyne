//! Fused value compilation: anchor pre-filter → cognition compiler.
//!
//! Combines the two measured channels into one pipeline:
//!
//! 1. **Anchor pre-filter** ([`crate::anchor::AnchorClassifier`]): every user
//!    message is embedded and compared against high-value category anchors
//!    (decision / goal / problem / plan). Messages below the threshold are
//!    dropped ("垃圾话" filter).
//! 2. **Compiler structuring** ([`CognitionCompiler::compile_conversation`]):
//!    the high-value messages are compiled into three-state facts (with
//!    evidence) — the coverage of anchors plus the structure of the compiler.
//! 3. **Raw retention**: messages the compiler cannot structure (its action
//!    markers don't match) are kept as raw [`HighValueItem`]s, so no
//!    information is lost to a narrow marker set.
//!
//! Deterministic, zero-LLM, evidence-anchored. Depends only on grayscale
//! modules (`anchor`, `value_extract`) plus the existing compiler.

use serde::Serialize;

use crate::anchor::AnchorClassifier;
use crate::cognition::Fact;
use crate::cognition_compiler::{CognitionCompileResult, CognitionCompiler};
use crate::embed::EmbeddingService;
use crate::error::Result;
use crate::types::Message;
use crate::value_extract::{HighValueItem, extract_high_value};

/// Summary counters for a fused compilation pass.
#[derive(Debug, Clone, Serialize)]
pub struct FuseSummary {
    /// Total input messages.
    pub total_messages: usize,
    /// User messages surfaced as high-value by the anchor filter.
    pub high_value: usize,
    /// Three-state facts produced by the compiler from high-value messages.
    pub facts_compiled: usize,
    /// High-value messages the compiler could NOT structure (kept raw).
    pub raw_items: usize,
    /// Category distribution of the high-value items.
    pub by_category: std::collections::BTreeMap<String, usize>,
}

/// Complete fused output: structured facts + raw high-value items.
#[derive(Debug, Clone, Serialize)]
pub struct FusedValueCompile {
    pub summary: FuseSummary,
    /// Three-state facts (with evidence), ready for persistence.
    pub facts: Vec<Fact>,
    /// High-value messages the compiler could not structure (original text,
    /// never lost).
    pub items: Vec<HighValueItem>,
}

/// Compiler-side context for a fused pass: where facts belong.
#[derive(Debug, Clone)]
pub struct FuseContext {
    /// Tenant scope for the compiled facts.
    pub tenant_id: String,
    /// Entity id the user's cognition is attributed to.
    pub user_entity_id: i64,
    /// Logical timestamp for the compiled facts.
    pub logical_time: i32,
}

/// Run the fused pipeline: anchor filter → compile → raw retention.
///
/// # Errors
///
/// Propagates embedder and compiler failures; returns an empty result when
/// the embedder is disabled (degradation, never a panic).
pub async fn fuse_value_compile(
    messages: &[Message],
    embedder: &dyn EmbeddingService,
    classifier: &AnchorClassifier,
    compiler: &CognitionCompiler,
    ctx: &FuseContext,
    threshold: f64,
) -> Result<FusedValueCompile> {
    // 1. Anchor pre-filter: high-value user messages.
    let high_value = extract_high_value(messages, embedder, classifier, threshold).await?;
    let total_messages = messages.len();

    // 2. Compiler structuring over ALL messages (it scans for action markers
    //    and emits evidence-anchored facts; the anchor filter decides what is
    //    "worth keeping", the compiler decides what is "structurable").
    let compiled: CognitionCompileResult = compiler.compile_conversation(
        &ctx.tenant_id,
        messages,
        ctx.user_entity_id,
        ctx.logical_time,
    );
    let facts = compiled.facts;

    // 3. Raw retention: high-value items that produced no compiled fact.
    //    Match by evidence text — a high-value message whose exact content
    //    appears in a compiled fact's evidence was structured; the rest are
    //    kept raw.
    let structured: std::collections::HashSet<String> = facts
        .iter()
        .filter_map(|f| {
            f.payload
                .get("evidence")
                .and_then(|e| e.get("text"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let raw_items: Vec<HighValueItem> = high_value
        .iter()
        .filter(|item| !structured.contains(&item.content))
        .cloned()
        .collect();

    // Category distribution for the summary.
    let mut by_category: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for item in &high_value {
        *by_category.entry(item.category.clone()).or_default() += 1;
    }

    Ok(FusedValueCompile {
        summary: FuseSummary {
            total_messages,
            high_value: high_value.len(),
            facts_compiled: facts.len(),
            raw_items: raw_items.len(),
            by_category,
        },
        facts,
        items: raw_items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value_extract::{AnchorSeeds, build_value_classifier};

    /// Deterministic test embedder (unigram + bigram hash), same contract as
    /// the one in `value_extract` tests: shared n-grams drive similarity.
    struct HashEmbedder;

    #[async_trait::async_trait]
    impl EmbeddingService for HashEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            let mut v = vec![0.0_f32; 128];
            let chars: Vec<char> = text.chars().collect();
            for c in &chars {
                v[(*c as usize) % 128] += 1.0;
            }
            for w in chars.windows(2) {
                let h = (w[0] as usize * 31 + w[1] as usize) % 128;
                v[h] += 1.5;
            }
            let norm = (v.iter().map(|x| x * x).sum::<f32>()).sqrt().max(1e-6);
            Ok(v.iter().map(|x| x / norm).collect())
        }
        async fn embed_with_prefix(&self, text: &str, _prefix: &str) -> Result<Vec<f32>> {
            self.embed(text).await
        }
        async fn health_check(&self) -> Result<()> {
            Ok(())
        }
        fn model(&self) -> &str {
            "hash-test"
        }
        fn timeout(&self) -> std::time::Duration {
            std::time::Duration::from_secs(1)
        }
        fn enabled(&self) -> bool {
            true
        }
    }

    fn seeds() -> AnchorSeeds {
        AnchorSeeds {
            decision: vec!["决定".into(), "灰度".into(), "一步到位".into()],
            goal: vec!["目标".into(), "计划".into(), "落地".into()],
            problem: vec!["问题".into(), "根因".into(), "报错".into()],
            plan: vec!["方案".into(), "步骤".into(), "下一步".into()],
        }
    }

    /// Objective: Verify compilable high-value messages become structured
    /// facts (compiler marks + anchor value both fire).
    /// Invariants: "我计划落地这个方案" contains the compiler marker 计划
    /// (→ plan/goal fact with evidence) and anchor goal/plan terms; the fused
    /// output has ≥1 fact whose evidence text matches the message.
    #[tokio::test]
    async fn compilable_messages_become_facts() {
        let embedder = HashEmbedder;
        let classifier = build_value_classifier(&seeds(), &embedder)
            .await
            .expect("build");
        let compiler = CognitionCompiler::default();
        let msgs = vec![Message::new("user", "我计划落地这个方案")];

        let fused = fuse_value_compile(
            &msgs,
            &embedder,
            &classifier,
            &compiler,
            &FuseContext {
                tenant_id: "t".into(),
                user_entity_id: 1,
                logical_time: 1000,
            },
            0.3,
        )
        .await
        .expect("fuse");
        assert!(
            !fused.facts.is_empty(),
            "compilable message must produce a fact, got summary {:?}",
            fused.summary
        );
        assert!(
            fused.facts.iter().any(|f| {
                f.payload
                    .get("evidence")
                    .and_then(|e| e.get("text"))
                    .and_then(|t| t.as_str())
                    .is_some_and(|s| s.contains("计划"))
            }),
            "fact evidence must anchor the compilable message"
        );
        // The message is structured → NOT kept raw.
        assert!(
            !fused.items.iter().any(|i| i.content.contains("计划")),
            "structured message must not duplicate as a raw item"
        );
    }

    /// Objective: Verify high-value messages the compiler cannot structure are
    /// retained as raw items (no information loss to a narrow marker set).
    /// Invariants: "遇到了报错，找根因" hits the problem anchor (报错/根因) but
    /// has no compiler action marker (喜欢/偏好/准备/打算/计划/want/压力…)
    /// → raw item present with original content.
    #[tokio::test]
    async fn non_compilable_kept_raw() {
        let embedder = HashEmbedder;
        let classifier = build_value_classifier(&seeds(), &embedder)
            .await
            .expect("build");
        let compiler = CognitionCompiler::default();
        let msgs = vec![Message::new("user", "遇到了报错，找根因")];

        let fused = fuse_value_compile(
            &msgs,
            &embedder,
            &classifier,
            &compiler,
            &FuseContext {
                tenant_id: "t".into(),
                user_entity_id: 1,
                logical_time: 1000,
            },
            0.3,
        )
        .await
        .expect("fuse");
        // Raw retention must be non-empty (message is high-value but has no
        // compiler marker — no observation/fact is produced for it).
        assert!(
            fused.items.iter().any(|i| i.content.contains("报错")),
            "non-compilable high-value message must be kept raw, got items {:?}",
            fused.items
        );
    }

    /// Objective: Verify low-value filler is dropped entirely (neither fact
    /// nor raw item).
    /// Invariants: "好的" is below the anchor threshold → absent from both
    /// `facts` and `items`.
    #[tokio::test]
    async fn low_value_dropped() {
        let embedder = HashEmbedder;
        let classifier = build_value_classifier(&seeds(), &embedder)
            .await
            .expect("build");
        let compiler = CognitionCompiler::default();
        let msgs = vec![Message::new("user", "好的"), Message::new("user", "继续")];

        let fused = fuse_value_compile(
            &msgs,
            &embedder,
            &classifier,
            &compiler,
            &FuseContext {
                tenant_id: "t".into(),
                user_entity_id: 1,
                logical_time: 1000,
            },
            0.3,
        )
        .await
        .expect("fuse");
        assert!(
            !fused
                .items
                .iter()
                .any(|i| i.content == "好的" || i.content == "继续"),
            "filler must be dropped from raw items, got {:?}",
            fused.items
        );
        assert!(
            fused.facts.iter().all(|f| {
                !f.payload
                    .get("evidence")
                    .and_then(|e| e.get("text"))
                    .and_then(|t| t.as_str())
                    .is_some_and(|s| s == "好的" || s == "继续")
            }),
            "filler must not appear in facts either"
        );
        assert_eq!(
            fused.summary.high_value, 0,
            "no high-value items expected for pure filler"
        );
    }
}
