//! MCP only: query 孔宣 fate.
//! Run: cargo test --test mcp_kongxuan -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

/// The fengshen DB is built by `fengshen_mcp_query`; CI runs tests in
/// parallel with no serial group, so this read-only demo must skip when the
/// DB is absent or mid-rebuild instead of panicking on open.
const DB: &str = "/tmp/fengshen_mcp.db";

#[tokio::test]
async fn mcp_kongxuan() {
    if !std::path::Path::new(DB).exists() {
        eprintln!("⚠  {DB} not present — skipping (built by fengshen_mcp_query)");
        return;
    }
    let k = Arc::new(match SQLiteKnowledgeStore::open(DB).await {
        Ok(k) => k,
        Err(e) => {
            eprintln!("⚠  {DB} not openable (parallel rebuild?): {e} — skipping");
            return;
        }
    });
    let hits = k.search_evidence("准提道人收孔宣", None, 5).await.unwrap();
    for h in &hits {
        println!("Ch.{}: {}", h.chapter, h.text);
    }
}
