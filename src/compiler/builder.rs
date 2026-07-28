//! ObjectBuilder + EdgeBuilder — Phase 6.
//!
//! Decomposes each [`Observation`] into [`CompiledObject`] and [`CompiledEdge`]
//! entries. An observation like `subject:赵云 predicate:救 object:[阿斗]
//! attributes:{mount:白马}` produces:
//!
//! - CompiledObject("赵云", person)
//! - CompiledObject("阿斗", person)
//! - CompiledObject("白马", artifact)
//! - CompiledEdge("赵云", "骑", "白马")
//! - CompiledEdge("赵云", "救", "阿斗")
//!
//! All entries carry an [`EvidenceSlice`] back to the source sentence.
//!
//! TODO: implementation stages will be filled in during Phase 6.

/// Placeholder.
pub fn build_objects() {}
