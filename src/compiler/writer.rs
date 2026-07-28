//! Writer — Phase 8.
//!
//! Converts a [`CompileResult`](crate::compiler::CompileResult) into storage
//! model types ([`KnowledgeObject`], [`KnowledgeEdge`], [`Evidence`]) and
//! writes them via a [`KnowledgeWriter`].
//!
//! The writer is the only component that knows about storage. It handles:
//! - entity_id assignment (UPSERT on name + doc_id)
//! - EvidenceSlice → Evidence row + knowledge_evidence link
//! - INSERT OR IGNORE for idempotent re-runs
//!
//! TODO: implementation stages will be filled in during Phase 8.

/// Placeholder.
pub async fn write() {}
