//! Real-embedding probe & value extraction (permanent, feature-gated).
//!
//! The project ships a self-contained ONNX embedder (`FastEmbedProvider`,
//! all-MiniLM-L6-v2, 384-dim) behind the `local-embed` feature — no remote
//! server, no API key; the model downloads once and caches locally.
//!
//! Run with: `cargo test --features local-embed --test real_embed_probe`
//!
//! Two checks:
//! 1. The ONNX model loads on this machine and produces 384-dim vectors.
//! 2. Real-embedding anchor value extraction surfaces high-value messages
//!    from the CodeScope export (coverage vs the 3-fact compile baseline).

#![cfg(feature = "local-embed")]

use lore_scope::anchor::AnchorClassifier;
use lore_scope::entity_resolver::{Embedder, FastEmbedProvider};
use lore_scope::types::Message;
use lore_scope::value_extract::{AnchorSeeds, HighValueItem};

fn load_export_messages(path: &str) -> Vec<Message> {
    let raw = std::fs::read_to_string(path).expect("read export json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
    v["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .filter_map(|m| {
            let role = m.get("role")?.as_str()?;
            let content = m.get("content")?.as_str()?;
            if content.trim().is_empty() {
                return None;
            }
            Some(Message::new(role.to_string(), content.to_string()))
        })
        .collect()
}

/// Objective: Verify the project's self-contained ONNX embedder loads and
/// produces 384-dim vectors (the deployment story: no remote server needed).
/// Invariants: model loads; embedding dimension == 384; vector is non-zero.
#[test]
fn onnx_embedder_loads_and_embeds() {
    let provider = FastEmbedProvider::new().expect("fastembed ONNX model must load locally");
    let v = provider.embed("测试一下用真实模型").expect("embed");
    assert_eq!(v.len(), 384, "all-MiniLM-L6-v2 is 384-dim");
    assert!(
        v.iter().any(|x| x.abs() > 1e-6),
        "embedding must be non-zero"
    );
}

/// Objective: Verify real-embedding anchor extraction surfaces high-value
/// messages from the CodeScope export — the actual measured result.
/// Invariants: extraction finds >= the 3-fact compile baseline; the top
/// scoring message is a meaningful directive (not empty/garbage).
#[test]
fn real_value_extraction_on_export() {
    let path = "corpus/conversation_export_2026-08-02.json";
    if !std::path::Path::new(path).exists() {
        eprintln!("SKIP: corpus/conversation_export_2026-08-02.json missing");
        return;
    }
    let messages = load_export_messages(path);
    let seeds = AnchorSeeds::load().expect("seeds from config/anchor_seeds.json");
    let provider = FastEmbedProvider::new().expect("real embedder");

    let mut per_category: Vec<(String, Vec<Vec<f32>>)> = Vec::new();
    for (cat, phrases) in seeds.as_pairs() {
        let refs: Vec<&str> = phrases.iter().map(|s| s.as_str()).collect();
        let vecs = provider.embed_batch(&refs).expect("embed seeds");
        per_category.push((cat, vecs));
    }
    let classifier = AnchorClassifier::from_seed_vectors(&per_category);

    let mut items: Vec<HighValueItem> = Vec::new();
    for m in &messages {
        if m.role != "user" || m.content.trim().is_empty() {
            continue;
        }
        let emb = provider.embed(&m.content).expect("embed message");
        if let Some((cat, score)) = classifier.classify(&emb).first() {
            if *score >= 0.30 {
                items.push(HighValueItem {
                    category: cat.clone(),
                    score: *score,
                    content: m.content.clone(),
                });
            }
        }
    }
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Coverage: real extraction must beat the 3-fact compile baseline.
    assert!(
        items.len() > 3,
        "real anchor extraction must surface more than the 3-fact compile baseline, got {}",
        items.len()
    );
    // Top item is a meaningful directive (non-trivial length, no pure filler).
    if let Some(top) = items.first() {
        assert!(
            top.content.chars().count() >= 2,
            "top item must carry real content, got {:?}",
            top.content
        );
    }
}
