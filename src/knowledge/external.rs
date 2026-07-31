//! External knowledge registry — runtime manager for attached external sources.
//!
//! The registry is the single point through which the rest of the crate talks
//! to external knowledge sources at query time. It holds two collections:
//!
//! - `adapters`: every registered [`KnowledgeAdapter`] (Document / Db / Vector).
//!   Used for materialization (`knowledge_ingest` materialize mode) and for
//!   aggregating cross-source [`EntityLink`]s.
//! - `signal_providers`: the subset of adapters that ALSO implement
//!   [`ExternalSignalProvider`] (Db / Vector only). These are query-forwarded:
//!   [`ExternalKnowledgeRegistry::search_all`] fans a query out to every
//!   provider and merges the hits by descending score.
//!
//! ## Design notes
//!
//! - Document adapters are materialization-only: they are NEVER queried at
//!   retrieval time (their content reaches the graph via the compiler, not via
//!   query forwarding — dev_guide "事实来自编译", "不双写").
//! - Db / Vector adapters are index-mode: they are queried but not materialized
//!   by default. The same concrete adapter is registered ONCE and coerced to
//!   both trait objects so the registry shares a single underlying allocation.
//! - The registry is `Send + Sync` (all fields are behind `Arc`), so it can be
//!   wrapped in `Arc` and shared between the MCP server and the retrieval
//!   engine.

use std::sync::{Arc, RwLock};

use crate::error::Result;
use crate::knowledge::adapter::{
    AdapterKind, EntityLink, ExternalDoc, ExternalHit, ExternalSignalProvider, IncrementalCursor,
    KnowledgeAdapter,
};

// ───────────────────────────────────────────────────────────────────────────
// ExternalKnowledgeRegistry
// ───────────────────────────────────────────────────────────────────────────

/// Runtime registry of attached external knowledge sources.
///
/// Created once at server start (and mutated by the `knowledge_attach` MCP
/// tool); shared with the [`crate::retrieval::RetrievalEngine`] via `Arc` so
/// hybrid search can fuse external signals into RRF.
///
/// Interior mutability: both vectors live behind a single [`RwLock`] so the
/// MCP `knowledge_attach` tool can register adapters at runtime (write lock)
/// while concurrent `memory_search` calls read the registry (read lock)
/// without rebuilding the engine or cloning the `Arc`.
pub struct ExternalKnowledgeRegistry {
    /// Every registered adapter, regardless of kind. Used for materialization
    /// and entity-link aggregation. Write-locked on register; read-locked on
    /// every query/materialize.
    adapters: RwLock<Vec<Arc<dyn KnowledgeAdapter>>>,
    /// The subset of adapters that also provide query-time signals (Db/Vector).
    /// Document adapters never appear here — they are materialized, not queried.
    signal_providers: RwLock<Vec<Arc<dyn ExternalSignalProvider>>>,
}

impl ExternalKnowledgeRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            adapters: RwLock::new(Vec::new()),
            signal_providers: RwLock::new(Vec::new()),
        }
    }

    /// Register a materialization-only adapter (Document kind).
    ///
    /// The adapter is stored for `materialize_all` / `collect_entity_links`
    /// but is NOT queryable — document content must flow through the compiler.
    ///
    /// Takes `&self` so the registry can be shared via `Arc` and mutated at
    /// runtime by the `knowledge_attach` MCP tool (write lock).
    pub fn register_document(&self, adapter: Arc<dyn KnowledgeAdapter>) {
        let mut guard = self
            .adapters
            .write()
            .expect("external registry adapters lock poisoned");
        guard.push(adapter);
    }

    /// Register an index-mode adapter that is BOTH a [`KnowledgeAdapter`] and
    /// an [`ExternalSignalProvider`] (i.e. a `DbAdapter` or `VectorAdapter`).
    ///
    /// The single concrete value is coerced to both trait objects so the
    /// registry references one underlying allocation from two trait-object
    /// `Arc`s. Document adapters (which are not signal providers) must use
    /// [`Self::register_document`] instead.
    ///
    /// Takes `&self` so the registry can be shared via `Arc` and mutated at
    /// runtime by the `knowledge_attach` MCP tool (write lock on both vectors).
    pub fn register_signal<T>(&self, adapter: T)
    where
        T: KnowledgeAdapter + ExternalSignalProvider + 'static,
    {
        let shared = Arc::new(adapter);
        // Two `Arc<T>` clones coerce to the two distinct trait objects; both
        // point at the same heap allocation so attach/detach stays consistent.
        let signal: Arc<dyn ExternalSignalProvider> = shared.clone();
        let knowledge: Arc<dyn KnowledgeAdapter> = shared;
        let mut adapters = self
            .adapters
            .write()
            .expect("external registry adapters lock poisoned");
        adapters.push(knowledge);
        let mut providers = self
            .signal_providers
            .write()
            .expect("external registry signal providers lock poisoned");
        providers.push(signal);
    }

    /// Returns `true` when no adapters are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.adapters
            .read()
            .expect("external registry adapters lock poisoned")
            .is_empty()
    }

    /// Number of registered adapters (document + signal).
    #[must_use]
    pub fn len(&self) -> usize {
        self.adapters
            .read()
            .expect("external registry adapters lock poisoned")
            .len()
    }

    /// Number of signal providers (Db/Vector only).
    #[must_use]
    pub fn signal_provider_count(&self) -> usize {
        self.signal_providers
            .read()
            .expect("external registry signal providers lock poisoned")
            .len()
    }

    /// Names of every registered source, in registration order.
    ///
    /// Returns owned `String`s because the adapters live behind a `RwLock` and
    /// borrowed `&str` cannot escape the read guard.
    #[must_use]
    pub fn source_names(&self) -> Vec<String> {
        self.adapters
            .read()
            .expect("external registry adapters lock poisoned")
            .iter()
            .map(|a| a.source_name().to_string())
            .collect()
    }

    /// Forward `query` to EVERY signal provider, merge all hits, and return the
    /// top `limit` by descending external score.
    ///
    /// Stable sort: providers queried in registration order; equal-score hits
    /// keep their provider order so results are deterministic across runs.
    #[must_use]
    pub fn search_all(&self, query: &str, limit: usize) -> Vec<ExternalHit> {
        if limit == 0 {
            return Vec::new();
        }
        // Hold the read lock only long enough to snapshot the provider list.
        // The actual search calls happen against the snapshot so a concurrent
        // `register_signal` does not deadlock waiting for our read lock.
        let providers: Vec<Arc<dyn ExternalSignalProvider>> = self
            .signal_providers
            .read()
            .expect("external registry signal providers lock poisoned")
            .clone();
        let mut all = Vec::new();
        for provider in &providers {
            all.extend(provider.search(query, limit));
        }
        all.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all.truncate(limit);
        all
    }

    /// Aggregate [`EntityLink`]s contributed by every registered adapter.
    ///
    /// Duplicates (same `external_name` + `canonical_name` + `source`) are
    /// removed so the entity linker receives a clean cross-source map.
    #[must_use]
    pub fn collect_entity_links(&self) -> Vec<EntityLink> {
        let adapters = self
            .adapters
            .read()
            .expect("external registry adapters lock poisoned");
        let mut links = Vec::new();
        for adapter in adapters.iter() {
            links.extend(adapter.entity_mapping());
        }
        links.sort_by(|a, b| {
            (&a.external_name, &a.canonical_name, &a.source).cmp(&(
                &b.external_name,
                &b.canonical_name,
                &b.source,
            ))
        });
        links.dedup_by(|a, b| {
            a.external_name == b.external_name
                && a.canonical_name == b.canonical_name
                && a.source == b.source
        });
        links
    }

    /// Materialize documents from every adapter using a fresh cursor.
    ///
    /// Used by `knowledge_ingest` (materialize mode). Each adapter is fetched
    /// once with an empty cursor so the full snapshot is returned; the caller
    /// (compiler pipeline) is responsible for persisting the resulting docs.
    ///
    /// # Errors
    ///
    /// Propagates any adapter `fetch_documents` error.
    pub fn materialize_all(&self) -> Result<Vec<ExternalDoc>> {
        let adapters = self
            .adapters
            .read()
            .expect("external registry adapters lock poisoned")
            .clone();
        let mut out = Vec::new();
        for adapter in &adapters {
            let (docs, _cursor) = adapter.fetch_documents(&IncrementalCursor::new())?;
            out.extend(docs);
        }
        Ok(out)
    }

    /// Materialize documents from a single named source.
    ///
    /// Returns [`crate::error::Error::NotFound`] when no adapter with `name` is
    /// registered. Used by `knowledge_ingest` when the caller targets one
    /// source rather than all.
    ///
    /// # Errors
    ///
    /// Propagates adapter errors; `NotFound` for an unknown source name.
    pub fn materialize_source(&self, name: &str) -> Result<Vec<ExternalDoc>> {
        let adapters = self
            .adapters
            .read()
            .expect("external registry adapters lock poisoned")
            .clone();
        for adapter in &adapters {
            if adapter.source_name() == name {
                return adapter
                    .fetch_documents(&IncrementalCursor::new())
                    .map(|(docs, _)| docs);
            }
        }
        Err(crate::error::Error::NotFound(format!(
            "external source `{name}` is not registered"
        )))
    }

    /// Returns the [`AdapterKind`] of a registered source, or `None`.
    #[must_use]
    pub fn kind_of(&self, name: &str) -> Option<AdapterKind> {
        self.adapters
            .read()
            .expect("external registry adapters lock poisoned")
            .iter()
            .find(|a| a.source_name() == name)
            .map(|a| a.adapter_kind())
    }

    /// Build an [`EntityLinker`] from the aggregated [`EntityLink`]s of every
    /// registered adapter (external-knowledge-plan §D2).
    ///
    /// The linker is a snapshot of the registry's current cross-source links.
    /// Callers rebuild it after attaching/detaching a source so `inspect_entity`
    /// sees a consistent view without holding a lock over the registry itself.
    #[must_use]
    pub fn build_entity_linker(&self) -> crate::knowledge::EntityLinker {
        crate::knowledge::EntityLinker::from_links(self.collect_entity_links())
    }
}

impl Default for ExternalKnowledgeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::adapter::{
        AdapterKind, DbAdapter, DocumentAdapter, EntityLink, ExternalDoc, ExternalHit,
        ExternalQueryFn, SchemaMapping, VectorAdapter,
    };

    /// Build a sample ExternalDoc for test fixtures.
    fn doc(title: &str, text: &str) -> ExternalDoc {
        ExternalDoc {
            title: title.into(),
            text: text.into(),
            chapter: None,
            source: "test".into(),
            doc_type: "text".into(),
            author: None,
        }
    }

    /// Objective: Verify a freshly-constructed registry reports empty state.
    /// Invariants: is_empty() true; len() 0; signal_provider_count() 0;
    /// source_names() empty; search_all returns nothing.
    #[test]
    fn new_registry_is_empty() {
        let reg = ExternalKnowledgeRegistry::new();
        assert!(reg.is_empty(), "fresh registry is empty");
        assert_eq!(reg.len(), 0, "no adapters");
        assert_eq!(reg.signal_provider_count(), 0, "no signal providers yet");
        assert!(reg.source_names().is_empty(), "no source names");
        assert!(
            reg.search_all("q", 10).is_empty(),
            "search on empty registry returns no hits"
        );
    }

    /// Objective: Verify register_document stores a Document adapter WITHOUT
    /// making it queryable (documents are materialized, not forwarded).
    /// Invariants: len 1, signal_provider_count 0, search_all empty,
    /// materialize_all returns the doc.
    #[test]
    fn register_document_is_materialize_only() {
        let reg = ExternalKnowledgeRegistry::new();
        let adapter = Arc::new(DocumentAdapter::new(
            "doc-src",
            vec![doc("ch1", "alpha"), doc("ch2", "beta")],
        ));
        reg.register_document(adapter);
        assert_eq!(reg.len(), 1, "adapter registered");
        assert_eq!(
            reg.signal_provider_count(),
            0,
            "document adapters are NOT signal providers"
        );
        assert_eq!(reg.source_names(), vec!["doc-src".to_string()]);
        // Documents must never be query-forwarded.
        assert!(
            reg.search_all("alpha", 5).is_empty(),
            "document adapter must not answer search_all"
        );
        // But it IS materializable.
        let docs = reg.materialize_all().expect("materialize");
        assert_eq!(docs.len(), 2, "both docs materialize once");
        assert_eq!(docs[0].title, "ch1");
    }

    /// Objective: Verify register_signal stores a Db adapter as BOTH a
    /// KnowledgeAdapter and an ExternalSignalProvider sharing one allocation.
    /// Invariants: len 1, signal_provider_count 1, search_all returns hits,
    /// kind_of returns Db.
    #[test]
    fn register_signal_makes_db_queryable() {
        let reg = ExternalKnowledgeRegistry::new();
        let hits = vec![ExternalHit {
            id: "row-1".into(),
            text: "lucky row".into(),
            score: 0.9,
            source: "fake-db".into(),
        }];
        let captured = hits.clone();
        let query_fn: ExternalQueryFn = Box::new(move |_q, _lim| captured.clone());
        let adapter = DbAdapter::new("fake-db", SchemaMapping::default(), query_fn);
        reg.register_signal(adapter);

        assert_eq!(reg.len(), 1, "one adapter registered");
        assert_eq!(
            reg.signal_provider_count(),
            1,
            "Db adapter is also a signal provider"
        );
        assert_eq!(reg.kind_of("fake-db"), Some(AdapterKind::Db));
        let got = reg.search_all("anything", 5);
        assert_eq!(got, hits, "search_all forwards to the Db adapter");
    }

    /// Objective: Verify search_all merges hits from MULTIPLE providers and
    /// returns them sorted by descending score, truncated to `limit`.
    /// Invariants: Two providers' hits interleave by score; limit truncates.
    #[test]
    fn search_all_merges_and_sorts_across_providers() {
        let reg = ExternalKnowledgeRegistry::new();
        // Provider A returns hits scored 0.3 and 0.7.
        let hits_a = vec![
            ExternalHit {
                id: "a-low".into(),
                text: "a low".into(),
                score: 0.3,
                source: "prov-a".into(),
            },
            ExternalHit {
                id: "a-high".into(),
                text: "a high".into(),
                score: 0.7,
                source: "prov-a".into(),
            },
        ];
        let captured_a = hits_a.clone();
        let adapter_a = DbAdapter::new(
            "prov-a",
            SchemaMapping::default(),
            Box::new(move |_, _| captured_a.clone()),
        );
        reg.register_signal(adapter_a);

        // Provider B returns hits scored 0.5 and 0.95.
        let hits_b = vec![
            ExternalHit {
                id: "b-mid".into(),
                text: "b mid".into(),
                score: 0.5,
                source: "prov-b".into(),
            },
            ExternalHit {
                id: "b-top".into(),
                text: "b top".into(),
                score: 0.95,
                source: "prov-b".into(),
            },
        ];
        let captured_b = hits_b.clone();
        let adapter_b = VectorAdapter::new("prov-b", Box::new(move |_, _| captured_b.clone()));
        reg.register_signal(adapter_b);

        let merged = reg.search_all("q", 10);
        assert_eq!(merged.len(), 4, "all hits merged");
        // Descending score order: 0.95, 0.7, 0.5, 0.3.
        assert_eq!(merged[0].id, "b-top");
        assert_eq!(merged[1].id, "a-high");
        assert_eq!(merged[2].id, "b-mid");
        assert_eq!(merged[3].id, "a-low");

        // Limit truncates after sorting.
        let top2 = reg.search_all("q", 2);
        assert_eq!(top2.len(), 2, "limit truncates merged list");
        assert_eq!(top2[0].id, "b-top", "highest score first");
        assert_eq!(top2[1].id, "a-high", "second highest second");
    }

    /// Objective: Verify search_all with limit=0 returns empty without calling
    /// providers (short-circuit).
    /// Invariants: limit=0 → empty vec.
    #[test]
    fn search_all_limit_zero_returns_empty() {
        let reg = ExternalKnowledgeRegistry::new();
        let adapter = DbAdapter::new(
            "db",
            SchemaMapping::default(),
            Box::new(|_, _| {
                vec![ExternalHit {
                    id: "x".into(),
                    text: "y".into(),
                    score: 1.0,
                    source: "db".into(),
                }]
            }),
        );
        reg.register_signal(adapter);
        assert!(
            reg.search_all("q", 0).is_empty(),
            "limit=0 short-circuits to empty"
        );
    }

    /// Objective: Verify collect_entity_links aggregates links from ALL adapters
    /// and dedups identical (external_name, canonical_name, source) triples.
    /// Invariants: Two adapters contribute overlapping links; dedup leaves one.
    #[test]
    fn collect_entity_links_aggregates_and_dedups() {
        let reg = ExternalKnowledgeRegistry::new();
        let link = EntityLink {
            external_name: "John Smith".into(),
            canonical_name: "Mr. Smith".into(),
            source: "crm".into(),
        };
        // Document adapter contributes the link.
        let doc_adapter =
            DocumentAdapter::new("doc-crm", vec![doc("d", "t")]).with_links(vec![link.clone()]);
        reg.register_document(Arc::new(doc_adapter));
        // Db adapter contributes the SAME link (dup) plus a distinct one.
        let distinct = EntityLink {
            external_name: "J. Smith".into(),
            canonical_name: "Mr. Smith".into(),
            source: "crm".into(),
        };
        let db_adapter = DbAdapter::new(
            "db-crm",
            SchemaMapping::default(),
            Box::new(|_, _| Vec::new()),
        )
        .with_links(vec![link.clone(), distinct.clone()]);
        reg.register_signal(db_adapter);

        let links = reg.collect_entity_links();
        // Two unique links after dedup; sorted by (external_name, canonical, source).
        assert_eq!(links.len(), 2, "duplicate link removed");
        // "J. Smith" sorts before "John Smith".
        assert_eq!(links[0].external_name, "J. Smith");
        assert_eq!(links[1].external_name, "John Smith");
    }

    /// Objective: Verify materialize_source targets one named source and
    /// returns NotFound for an unknown name (no panic).
    /// Invariants: Known source returns its docs; unknown source → Err(NotFound).
    #[test]
    fn materialize_source_targets_named_adapter() {
        let reg = ExternalKnowledgeRegistry::new();
        reg.register_document(Arc::new(DocumentAdapter::new(
            "docs",
            vec![doc("only", "content")],
        )));

        let docs = reg.materialize_source("docs").expect("known source");
        assert_eq!(docs.len(), 1, "named source materializes its docs");
        assert_eq!(docs[0].title, "only");

        let err = reg.materialize_source("nope").unwrap_err();
        assert!(
            matches!(err, crate::error::Error::NotFound(_)),
            "unknown source must return NotFound, got {err:?}"
        );
    }

    /// Objective: Verify Default trait yields an empty registry equivalent to new().
    /// Invariants: default().is_empty() == true.
    #[test]
    fn default_is_empty() {
        let reg = ExternalKnowledgeRegistry::default();
        assert!(reg.is_empty(), "default registry is empty");
    }
}
