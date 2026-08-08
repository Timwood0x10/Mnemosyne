//! External knowledge adapter — unified entry for documents, DBs, and vector stores.
//!
//! Leaf module: defines the [`KnowledgeAdapter`] trait (materialization path)
//! and the [`ExternalSignalProvider`] trait (index-mode query path). Both traits
//! live here so that `retrieval.rs` (RRF fusion) and `knowledge/external.rs`
//! (registry) can depend on this leaf without creating a circular dependency
//! between the retrieval and knowledge modules.
//!
//! ## Decision rule (external-knowledge-plan §3)
//!
//! - **Document adapters** are full-snapshot: `fetch_documents` returns every
//!   document once (the cursor dedups by id); they are NOT signal providers
//!   because documents are materialized through the compiler, not query-forwarded.
//! - **DB / Vector adapters** are index-mode: `search` forwards the query to the
//!   external system and returns hits as retrieval signals; they never
//!   materialize by default (dev_guide §6 "不双写").

use std::collections::HashMap;
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::Result;

// ───────────────────────────────────────────────────────────────────────────
// Core data structures
// ───────────────────────────────────────────────────────────────────────────

/// One document emitted by an external source, ready for the compiler pipeline.
///
/// `source` is the origin identifier (file path, DB table, URL) used for
/// provenance; `doc_type` guides downstream chunking (`text`/`markdown`/`json`/
/// `pdf`/`db`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalDoc {
    pub title: String,
    pub text: String,
    /// Optional chapter number when the source is already segmented.
    pub chapter: Option<i32>,
    pub source: String,
    pub doc_type: String,
    pub author: Option<String>,
}

/// Declarative mapping from an external table's columns to internal IR fields.
///
/// Used as metadata for [`DbAdapter`] and as the contract for future
/// materialization-mode ingestion (external-knowledge-plan §B, "DB 物化型").
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SchemaMapping {
    /// External table name.
    pub table: String,
    /// Column name → internal IR field name (e.g. `"body" -> "text"`).
    pub columns: HashMap<String, String>,
    /// Timestamp column used as the incremental watermark (DB adapters only).
    pub watermark_column: Option<String>,
    /// Primary-key column used for `doc_id` dedup (DB adapters only).
    pub id_column: String,
}

/// Incremental fetch state.
///
/// DB adapters advance `watermark` (high-water mark of the watermark column)
/// and dedup via `seen_ids`; **document adapters are full-snapshot** — they
/// ignore the watermark and return every document once, then nothing on the
/// next call (the cursor records what has been emitted).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IncrementalCursor {
    /// High-water mark of the source's watermark column (`None` = unbounded).
    pub watermark: Option<i64>,
    /// External ids already emitted; used to dedup across fetches.
    pub seen_ids: HashSet<String>,
}

impl IncrementalCursor {
    /// Create an empty cursor (no watermark, nothing seen).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the cursor: raise the watermark to the max of old/new and record
    /// newly seen ids. Idempotent — re-advancing with the same ids is a no-op.
    pub fn advance(&mut self, watermark: Option<i64>, new_ids: impl IntoIterator<Item = String>) {
        if let Some(w) = watermark {
            self.watermark = Some(self.watermark.map_or(w, |old| old.max(w)));
        }
        for id in new_ids {
            self.seen_ids.insert(id);
        }
    }

    /// Returns `true` if `id` has already been emitted in a previous fetch.
    #[must_use]
    pub fn has_seen(&self, id: &str) -> bool {
        self.seen_ids.contains(id)
    }
}

/// A cross-source entity link: an external surface name resolves to a canonical
/// graph entity. Collected by [`KnowledgeAdapter::entity_mapping`] and consumed
/// by [`crate::knowledge::entity_linker::EntityLinker`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityLink {
    pub external_name: String,
    pub canonical_name: String,
    pub source: String,
}

/// Kind of external source. Drives whether the adapter is materialized
/// (Document) or query-forwarded (Db/Vector).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterKind {
    Document,
    Db,
    Vector,
}

// ───────────────────────────────────────────────────────────────────────────
// Adapter traits
// ───────────────────────────────────────────────────────────────────────────

/// Unified materialization entry for every external source.
///
/// Implementations convert their native rows/files into [`ExternalDoc`]s so the
/// existing compiler pipeline can take over (dev_guide "事实来自编译"). The
/// adapter never writes to the graph directly — that is the compiler's job.
pub trait KnowledgeAdapter: Send + Sync {
    /// Stable identifier for this source (used in provenance + EntityLinker keys).
    fn source_name(&self) -> &str;
    /// What kind of source this is.
    fn adapter_kind(&self) -> AdapterKind;
    /// Fetch the next batch of documents relative to `cursor`. Returns the
    /// fetched docs and the advanced cursor (callers persist the cursor to
    /// make the next fetch incremental).
    fn fetch_documents(
        &self,
        cursor: &IncrementalCursor,
    ) -> Result<(Vec<ExternalDoc>, IncrementalCursor)>;
    /// Cross-source entity links this source contributes. Empty by default.
    fn entity_mapping(&self) -> Vec<EntityLink> {
        Vec::new()
    }
}

/// Query-time signal provider for index-mode sources (DB / Vector).
///
/// Hits returned here are fused into hybrid retrieval as **synthetic RRF
/// candidates** (see `retrieval.rs`): they are not materialized and do not need
/// a matching local `Experience.id`.
pub trait ExternalSignalProvider: Send + Sync {
    /// Forward `query` to the external system and return up to `limit` hits,
    /// ranked by the external system's own score (descending).
    fn search(&self, query: &str, limit: usize) -> Vec<ExternalHit>;
}

/// One retrieval hit from an external index-mode source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalHit {
    /// External id (opaque to Mnemosyne).
    pub id: String,
    /// Snippet text used as the synthetic experience content.
    pub text: String,
    /// External relevance score in `[0.0, 1.0]` (higher = better).
    pub score: f64,
    /// Source name (mirrors [`KnowledgeAdapter::source_name`]).
    pub source: String,
}

/// Shared closure type for index-mode query/similarity functions.
///
/// Centralized as a type alias so `DbAdapter` and `VectorAdapter` signatures
/// stay readable (clippy `type_complexity`) and remain interchangeable.
pub type ExternalQueryFn = Box<dyn Fn(&str, usize) -> Vec<ExternalHit> + Send + Sync>;

// ───────────────────────────────────────────────────────────────────────────
// DocumentAdapter — wraps pre-loaded ExternalDocs (PDF/JSON/TXT/MD)
// ───────────────────────────────────────────────────────────────────────────

/// Adapter over an in-memory batch of [`ExternalDoc`]s produced by a format
/// loader. Full-snapshot: emits each doc once, then nothing.
///
/// Not an [`ExternalSignalProvider`]: documents are materialized through the
/// compiler, not query-forwarded.
pub struct DocumentAdapter {
    name: String,
    docs: Vec<ExternalDoc>,
    links: Vec<EntityLink>,
}

impl DocumentAdapter {
    /// Build a document adapter named `name` over `docs`.
    #[must_use]
    pub fn new(name: impl Into<String>, docs: Vec<ExternalDoc>) -> Self {
        Self {
            name: name.into(),
            docs,
            links: Vec::new(),
        }
    }

    /// Attach cross-source entity links contributed by this document source.
    #[must_use]
    pub fn with_links(mut self, links: Vec<EntityLink>) -> Self {
        self.links = links;
        self
    }
}

impl KnowledgeAdapter for DocumentAdapter {
    fn source_name(&self) -> &str {
        &self.name
    }

    fn adapter_kind(&self) -> AdapterKind {
        AdapterKind::Document
    }

    fn fetch_documents(
        &self,
        cursor: &IncrementalCursor,
    ) -> Result<(Vec<ExternalDoc>, IncrementalCursor)> {
        let mut next = cursor.clone();
        let mut out = Vec::new();
        for doc in &self.docs {
            // Dedup by title (document adapters have no natural numeric id).
            let key = doc.title.clone();
            if next.has_seen(&key) {
                continue;
            }
            out.push(doc.clone());
            next.seen_ids.insert(key);
        }
        Ok((out, next))
    }

    fn entity_mapping(&self) -> Vec<EntityLink> {
        self.links.clone()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// DbAdapter — index-mode SQL source (dependency-free via injected query fn)
// ───────────────────────────────────────────────────────────────────────────

/// Index-mode DB adapter. Real SQL connectivity is a future implementation
/// behind the same [`KnowledgeAdapter`] trait; for now the query behavior is
/// injected as a closure so the adapter is testable without a heavy `sqlx`
/// dependency (rules.md: no network, fast builds).
///
/// `fetch_documents` materializes by issuing a broad query (empty string) and
/// converting hits to [`ExternalDoc`]s — used only when `knowledge_ingest` is
/// called with `mode=materialize`. The default index-mode path goes through
/// [`ExternalSignalProvider::search`] and never materializes.
pub struct DbAdapter {
    name: String,
    mapping: SchemaMapping,
    query_fn: ExternalQueryFn,
    links: Vec<EntityLink>,
}

impl DbAdapter {
    /// Build a DB adapter with a query closure and schema mapping.
    #[must_use]
    pub fn new(name: impl Into<String>, mapping: SchemaMapping, query_fn: ExternalQueryFn) -> Self {
        Self {
            name: name.into(),
            mapping,
            query_fn,
            links: Vec::new(),
        }
    }

    /// Attach cross-source entity links contributed by this DB source.
    #[must_use]
    pub fn with_links(mut self, links: Vec<EntityLink>) -> Self {
        self.links = links;
        self
    }

    /// Read-only access to the schema mapping (for `knowledge_ingest`/UI).
    #[must_use]
    pub fn mapping(&self) -> &SchemaMapping {
        &self.mapping
    }
}

impl KnowledgeAdapter for DbAdapter {
    fn source_name(&self) -> &str {
        &self.name
    }

    fn adapter_kind(&self) -> AdapterKind {
        AdapterKind::Db
    }

    fn fetch_documents(
        &self,
        cursor: &IncrementalCursor,
    ) -> Result<(Vec<ExternalDoc>, IncrementalCursor)> {
        // Broad query (empty string) returns all rows the external system will
        // volunteer; convert hits to ExternalDocs for materialization mode.
        let hits = (self.query_fn)("", usize::MAX);
        let mut next = cursor.clone();
        let mut out = Vec::new();
        for hit in hits {
            if next.has_seen(&hit.id) {
                continue;
            }
            out.push(ExternalDoc {
                title: hit.id.clone(),
                text: hit.text,
                chapter: None,
                source: self.name.clone(),
                doc_type: "db".into(),
                author: None,
            });
            next.seen_ids.insert(hit.id);
        }
        Ok((out, next))
    }

    fn entity_mapping(&self) -> Vec<EntityLink> {
        self.links.clone()
    }
}

impl ExternalSignalProvider for DbAdapter {
    fn search(&self, query: &str, limit: usize) -> Vec<ExternalHit> {
        (self.query_fn)(query, limit)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// VectorAdapter — index-mode external vector store (no re-embedding)
// ───────────────────────────────────────────────────────────────────────────

/// Index-mode vector adapter. Reuses an external vector store via an injected
/// similarity closure — Mnemosyne never re-computes embeddings for the source.
pub struct VectorAdapter {
    name: String,
    similarity_fn: ExternalQueryFn,
    links: Vec<EntityLink>,
}

impl VectorAdapter {
    /// Build a vector adapter with a similarity closure.
    #[must_use]
    pub fn new(name: impl Into<String>, similarity_fn: ExternalQueryFn) -> Self {
        Self {
            name: name.into(),
            similarity_fn,
            links: Vec::new(),
        }
    }

    /// Attach cross-source entity links contributed by this vector source.
    #[must_use]
    pub fn with_links(mut self, links: Vec<EntityLink>) -> Self {
        self.links = links;
        self
    }
}

impl KnowledgeAdapter for VectorAdapter {
    fn source_name(&self) -> &str {
        &self.name
    }

    fn adapter_kind(&self) -> AdapterKind {
        AdapterKind::Vector
    }

    fn fetch_documents(
        &self,
        cursor: &IncrementalCursor,
    ) -> Result<(Vec<ExternalDoc>, IncrementalCursor)> {
        let hits = (self.similarity_fn)("", usize::MAX);
        let mut next = cursor.clone();
        let mut out = Vec::new();
        for hit in hits {
            if next.has_seen(&hit.id) {
                continue;
            }
            out.push(ExternalDoc {
                title: hit.id.clone(),
                text: hit.text,
                chapter: None,
                source: self.name.clone(),
                doc_type: "vector".into(),
                author: None,
            });
            next.seen_ids.insert(hit.id);
        }
        Ok((out, next))
    }

    fn entity_mapping(&self) -> Vec<EntityLink> {
        self.links.clone()
    }
}

impl ExternalSignalProvider for VectorAdapter {
    fn search(&self, query: &str, limit: usize) -> Vec<ExternalHit> {
        (self.similarity_fn)(query, limit)
    }
}

// `Arc<dyn KnowledgeAdapter>` and `Arc<dyn ExternalSignalProvider>` are stored
// separately by `knowledge/external.rs::ExternalKnowledgeRegistry`: a concrete
// `DbAdapter`/`VectorAdapter` is coerced to both trait objects at attach time
// (Arc<T> coerces to each supertait T implements). Document adapters are
// attached only as `KnowledgeAdapter` since they are materialized, not queried.

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify ExternalDoc serializes and deserializes losslessly.
    /// Invariants: A round-trip through serde_json preserves every field.
    #[test]
    fn external_doc_serde_round_trip() {
        let doc = ExternalDoc {
            title: "Report".into(),
            text: "Body text".into(),
            chapter: Some(3),
            source: "report.pdf".into(),
            doc_type: "pdf".into(),
            author: Some("Jane".into()),
        };
        let json = serde_json::to_string(&doc).expect("serialize");
        let back: ExternalDoc = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, doc, "round-trip must preserve all fields");
    }

    /// Objective: Verify SchemaMapping::default yields empty maps/columns.
    /// Invariants: Default mapping has empty table, no columns, no watermark.
    #[test]
    fn schema_mapping_default_is_empty() {
        let m = SchemaMapping::default();
        assert!(m.table.is_empty(), "default table is empty");
        assert!(m.columns.is_empty(), "default columns are empty");
        assert!(m.watermark_column.is_none(), "default watermark is None");
        assert!(m.id_column.is_empty(), "default id_column is empty");
    }

    /// Objective: Verify IncrementalCursor::advance raises the watermark and
    /// dedups ids.
    /// Invariants: Watermark is the max of old/new; seen_ids accumulates;
    /// has_seen returns true only for recorded ids.
    #[test]
    fn cursor_advance_dedups_and_raises_watermark() {
        let mut c = IncrementalCursor::new();
        c.advance(Some(10), ["a".to_string(), "b".to_string()]);
        assert_eq!(c.watermark, Some(10), "first advance sets watermark");
        assert!(c.has_seen("a") && c.has_seen("b"));
        assert!(!c.has_seen("c"));
        // A lower watermark must not lower the high-water mark.
        c.advance(Some(5), ["a".to_string()]);
        assert_eq!(c.watermark, Some(10), "watermark is monotonic");
        assert_eq!(
            c.seen_ids.len(),
            2,
            "duplicate id 'a' does not grow the set"
        );
    }

    /// Objective: Verify DocumentAdapter emits each doc exactly once across
    /// fetches (full-snapshot semantics).
    /// Invariants: First fetch returns all docs; second fetch returns none.
    #[test]
    fn document_adapter_emits_each_doc_once() {
        let docs = vec![
            ExternalDoc {
                title: "ch1".into(),
                text: "alpha".into(),
                chapter: Some(1),
                source: "doc".into(),
                doc_type: "text".into(),
                author: None,
            },
            ExternalDoc {
                title: "ch2".into(),
                text: "beta".into(),
                chapter: Some(2),
                source: "doc".into(),
                doc_type: "text".into(),
                author: None,
            },
        ];
        let adapter = DocumentAdapter::new("doc-src", docs.clone());
        let (out1, cursor) = adapter
            .fetch_documents(&IncrementalCursor::new())
            .expect("fetch1");
        assert_eq!(out1.len(), 2, "first fetch returns both docs");
        let (out2, _) = adapter.fetch_documents(&cursor).expect("fetch2");
        assert!(
            out2.is_empty(),
            "second fetch returns nothing (already seen)"
        );
    }

    /// Objective: Verify DbAdapter forwards queries to its closure and
    /// implements ExternalSignalProvider.
    /// Invariants: search returns hits in closure-defined order; ids survive.
    #[test]
    fn db_adapter_forwards_search() {
        let hits = vec![
            ExternalHit {
                id: "row-7".into(),
                text: "lucky row".into(),
                score: 0.9,
                source: "fake-db".into(),
            },
            ExternalHit {
                id: "row-3".into(),
                text: "other row".into(),
                score: 0.4,
                source: "fake-db".into(),
            },
        ];
        let captured = hits.clone();
        let adapter = DbAdapter::new(
            "fake-db",
            SchemaMapping::default(),
            Box::new(move |_q, _lim| captured.clone()),
        );
        let got = adapter.search("anything", 5);
        assert_eq!(got, hits, "search must echo the closure's hits");
        assert_eq!(adapter.adapter_kind(), AdapterKind::Db);
    }

    /// Objective: Verify DbAdapter fetch_documents dedups across fetches.
    /// Invariants: First fetch materializes 2 docs; second fetch yields 0.
    #[test]
    fn db_adapter_fetch_dedups() {
        let hits = vec![
            ExternalHit {
                id: "1".into(),
                text: "one".into(),
                score: 0.5,
                source: "db".into(),
            },
            ExternalHit {
                id: "2".into(),
                text: "two".into(),
                score: 0.5,
                source: "db".into(),
            },
        ];
        let captured = hits.clone();
        let adapter = DbAdapter::new(
            "db",
            SchemaMapping::default(),
            Box::new(move |_, _| captured.clone()),
        );
        let (out1, cursor) = adapter
            .fetch_documents(&IncrementalCursor::new())
            .expect("f1");
        assert_eq!(out1.len(), 2);
        assert_eq!(out1[0].doc_type, "db", "materialized docs are tagged db");
        let (out2, _) = adapter.fetch_documents(&cursor).expect("f2");
        assert!(out2.is_empty(), "second fetch dedups against the cursor");
    }

    /// Objective: Verify VectorAdapter implements ExternalSignalProvider and
    /// surfaces its kind (exercised to avoid dead_code under --all-features).
    /// Invariants: search returns the closure's hits; kind is Vector.
    #[test]
    fn vector_adapter_provides_signal() {
        let hits = vec![ExternalHit {
            id: "v1".into(),
            text: "near neighbor".into(),
            score: 0.88,
            source: "vec-store".into(),
        }];
        let captured = hits.clone();
        let adapter = VectorAdapter::new("vec-store", Box::new(move |_, _| captured.clone()));
        assert_eq!(adapter.adapter_kind(), AdapterKind::Vector);
        let got = adapter.search("q", 3);
        assert_eq!(got, hits);
        // entity_mapping defaults to empty when no links are attached.
        assert!(adapter.entity_mapping().is_empty());
    }

    /// Objective: Verify entity links attached via with_links are returned by
    /// entity_mapping for DocumentAdapter.
    /// Invariants: Links survive attachment and round-trip through the trait.
    #[test]
    fn document_adapter_carries_entity_links() {
        let links = vec![EntityLink {
            external_name: "John Smith".into(),
            canonical_name: "Mr. Smith".into(),
            source: "crm".into(),
        }];
        let adapter = DocumentAdapter::new("crm", Vec::new()).with_links(links.clone());
        assert_eq!(adapter.entity_mapping(), links);
    }
}
