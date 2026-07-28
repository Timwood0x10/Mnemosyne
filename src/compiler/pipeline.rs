//! Pipeline orchestrator — the top-level `compile()` entry point.
//!
//! Runs the full pipeline: Text → Document → ChunkPlanner → Sentence → Entity
//! → AliasResolver → PronounResolver → Observation → ObjectBuilder+EdgeBuilder
//! → Merge → Rule → Writer.
//!
//! Each phase reads from (and writes to) a shared [`CompileContext`].
//! The context flows through all phases; nothing is persisted until the
//! final [`Writer`](crate::compiler::writer) stage.
//!
//! ## Usage
//!
//! ```ignore
//! use lore_scope::compiler::pipeline;
//!
//! let ctx = pipeline::from_file("corpus/三国演义.txt").await?;
//! // ctx.result now contains the CompileResult
//! ```

use crate::compiler::document::Document;
use crate::compiler::chunk;
use crate::compiler::CompileContext;
use crate::error::Result;

/// Compile a text file into a [`CompileContext`].
///
/// Shortcut for: load file → create context → run all phases.
pub async fn from_file(path: impl AsRef<std::path::Path>) -> Result<CompileContext> {
    let doc = Document::from_file(path).map_err(|e| {
        crate::error::distillation_error("document", format!("load file: {e}"))
    })?;
    from_document(doc).await
}

/// Compile a raw text string.
pub fn from_text(title: &str, doc_type: &str, text: &str) -> CompileContext {
    let doc = Document::from_text(title, doc_type, text);
    let mut ctx = CompileContext::default();
    ctx.document = Some(doc);
    // Phase 1: chunk (sequential, for now)
    let doc = ctx.document.as_ref().unwrap();
    ctx.chunks = chunk::plan(doc.text.as_str(), chunk::Config::default());
    ctx
}

/// Compile from message slices (conversation input).
pub fn from_messages(messages: &[crate::types::Message]) -> CompileContext {
    let doc = Document::from_messages(messages);
    from_text(&doc.title, &doc.doc_type, &doc.text)
}

/// Compile from an already-loaded [`Document`].
pub async fn from_document(doc: Document) -> Result<CompileContext> {
    let mut ctx = CompileContext::default();
    ctx.document = Some(doc);
    // Phase 1: chunk
    let doc = ctx.document.as_ref().unwrap();
    ctx.chunks = chunk::plan(doc.text.as_str(), chunk::Config::default());
    // Future phases will be added here as they are implemented.
    Ok(ctx)
}

/// Run all remaining phases on an already-initialized context.
///
/// This is a no-op until later phases are wired in.
pub async fn run_all(ctx: &mut CompileContext) -> Result<()> {
    // Phase 2: sentence  — TODO
    // Phase 3: entity    — TODO
    // Phase 4: alias     — TODO
    // Phase 5: pronoun   — TODO
    // Phase 6: builder   — TODO
    // Phase 7: merge     — TODO
    // Phase 8: rule      — TODO
    // Phase 9: writer    — TODO
    let _ = ctx;
    Ok(())
}
