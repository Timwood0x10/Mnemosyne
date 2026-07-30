//! VectorBuilder — standalone lifecycle for building vector indices.
//!
//! Separates the embedding + indexing pipeline from the compiler, so that
//! the compiler never needs to know about embeddings. The builder takes
//! compiler output (entities + events) and produces a queryable
//! [`Arc<dyn VectorIndex>`] ready for the resolver pipeline.
//!
//! ## Lifecycle
//!
//! ```text
//! CompileResult
//!       ↓
//! VectorBuilder::build()
//!   ├── 1. Build Entity Representations (text → EntityDocument)
//!   ├── 2. Embed all documents (EntityDocument → Vec<f32>)
//!   ├── 3. Build VectorIndex (vectors → HnswMap)
//!   └── 4. Return Arc<dyn VectorIndex>
//! ```

use std::sync::Arc;

use crate::compiler::CompileContext;
use crate::entity_resolver::Embedder;
use crate::error::Error;
use crate::vector::VectorIndex;

/// Builder that takes compiler output and produces a queryable vector index.
///
/// # Example
///
/// ```ignore
/// let builder = VectorBuilder::new(embedder, representation);
/// let index = builder.build(&ctx).await?;
/// ```
pub struct VectorBuilder {
    embedder: Arc<dyn Embedder>,
    max_events: usize,
}

impl VectorBuilder {
    /// Create a new vector builder with the given embedder.
    pub fn new(embedder: Arc<dyn Embedder>) -> Self {
        VectorBuilder {
            embedder,
            max_events: 20,
        }
    }

    /// Set the maximum number of events to include per entity representation.
    pub fn with_max_events(mut self, n: usize) -> Self {
        self.max_events = n;
        self
    }

    /// Build an entity representation string for a single entity.
    ///
    /// Template (fixed order, name × 3 for weight):
    /// ```text
    /// Name:<name>
    /// Name:<name>
    /// Name:<name>
    /// Alias:<alias1>,<alias2>
    /// Relations:<entity1>,<entity2>
    /// Events:<event1>,<event2>
    /// ```
    fn build_representation(
        &self,
        entity_name: &str,
        _entity_type: &str,
        aliases: &[String],
        relations: &[(String, String)],
        events: &[(String, i32, f64)],
    ) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(32);

        // Name × 3 (importance weighting)
        for _ in 0..3 {
            parts.push(format!("Name:{}", entity_name));
        }

        // Aliases
        if !aliases.is_empty() {
            parts.push(format!("Alias:{}", aliases.join(",")));
        }

        // Relations
        if !relations.is_empty() {
            let rel_names: Vec<&str> = relations.iter().map(|(n, _)| n.as_str()).collect();
            parts.push(format!("Relations:{}", rel_names.join(",")));
        }

        // Events (top N by importance)
        let mut sorted = events.to_vec();
        sorted.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let top: Vec<&str> = sorted
            .iter()
            .take(self.max_events)
            .map(|(t, _, _)| t.as_str())
            .collect();
        if !top.is_empty() {
            parts.push(format!("Events:{}", top.join(",")));
        }

        parts.join("\n")
    }

    /// Build the vector index from compiler output.
    ///
    /// Iterates over all discovered entities, builds their text
    /// representations, embeds them, and indexes them.
    pub fn build<T: VectorIndex + Default + 'static>(
        &self,
        ctx: &CompileContext,
    ) -> Result<Arc<dyn VectorIndex>, Error> {
        let mut items: Vec<(i64, Vec<f32>)> = Vec::new();

        for entity in &ctx.entities {
            let eid = entity.id.unwrap_or(0);
            if eid == 0 {
                continue;
            }

            // Collect aliases, relations, and events for this entity
            let aliases: Vec<String> = ctx
                .profiles
                .iter()
                .filter(|p| p.entity_id == entity.id)
                .filter(|p| p.key == "courtesy_name" || p.key == "title")
                .map(|p| p.value.clone())
                .collect();

            let relations: Vec<(String, String)> = ctx
                .events
                .iter()
                .filter(|ev| ev.participants.iter().any(|p| p.entity_name == entity.name))
                .flat_map(|ev| {
                    ev.participants
                        .iter()
                        .map(|p| (p.entity_name.clone(), ev.event_type.clone()))
                })
                .filter(|(n, _)| n != &entity.name)
                .collect();

            let events: Vec<(String, i32, f64)> = ctx
                .events
                .iter()
                .filter(|ev| ev.participants.iter().any(|p| p.entity_name == entity.name))
                .map(|ev| (ev.title.clone(), ev.timestamp.unwrap_or(0), ev.importance))
                .collect();

            let doc = self.build_representation(
                &entity.name,
                &entity.entity_type,
                &aliases,
                &relations,
                &events,
            );

            let vec = self.embedder.embed(&doc)?;
            items.push((eid, vec));
        }

        let mut index = T::default();
        index.build(&items)?;
        Ok(Arc::new(index))
    }
}
