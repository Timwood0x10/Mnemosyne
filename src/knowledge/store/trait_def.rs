//! The `KnowledgeStore` trait: the storage contract for the knowledge graph.

use super::*;

/// Backend-agnostic knowledge store contract.
///
/// All create methods return the new row's `id` so callers can chain inserts
/// (object → edge → evidence → link) without re-querying.
#[async_trait]
pub trait KnowledgeStore: Send + Sync {
    // ── documents / chapters ───────────────────────────────────
    async fn create_document(&self, d: &Document) -> Result<i64>;
    async fn find_document_by_title(&self, title: &str) -> Result<Option<Document>>;
    async fn create_chapter(&self, c: &Chapter) -> Result<i64>;
    async fn get_chapter_by_no(&self, doc_id: i64, chapter_no: i32) -> Result<Option<Chapter>>;

    // ── objects ───────────────────────────────────────────────
    async fn create_object(&self, o: &KnowledgeObject) -> Result<i64>;
    async fn get_object(&self, id: i64) -> Result<Option<KnowledgeObject>>;
    /// Fetch many objects by id in one query. Ids missing from the store are
    /// silently skipped (like a per-id `get_object` loop). Empty input returns
    /// an empty vec without touching the DB.
    async fn get_objects_bulk(&self, ids: &[i64]) -> Result<Vec<KnowledgeObject>>;
    async fn find_object_by_name(
        &self,
        name: &str,
        doc_id: Option<i64>,
    ) -> Result<Option<KnowledgeObject>>;
    /// Resolve an entity by exact name first, then by **name substring** as a
    /// fallback.
    ///
    /// Corpus extraction can store a given name ("流苏") while a caller queries
    /// the full name ("白流苏"), or vice versa. This method lets query tools
    /// (`inspect_entity`, `person_key_events`) find the same graph node through
    /// either spelling. The exact match is authoritative; the substring
    /// fallback only fires when no exact object exists, and it requires the
    /// substring candidate to be unambiguous to avoid alias false-positives.
    async fn find_object_by_alias(
        &self,
        name: &str,
        doc_id: Option<i64>,
    ) -> Result<Option<KnowledgeObject>>;
    /// Merge new `properties` (and optionally raise `confidence`) into an
    /// existing object, preserving any keys not overwritten. Returns the
    /// number of rows actually updated (0 when `id` is unknown).
    async fn update_object_properties(
        &self,
        id: i64,
        properties: &serde_json::Value,
        confidence: Option<f64>,
    ) -> Result<usize>;

    // ── edges ─────────────────────────────────────────────────
    async fn create_edge(&self, e: &KnowledgeEdge) -> Result<i64>;
    async fn get_edges_touching(&self, object_id: i64) -> Result<Vec<KnowledgeEdge>>;
    /// Re-target an edge to point at `new_target_id`. Used by the
    /// `correct_relation` MCP tool to fix misattributed relations in place.
    /// Returns the number of rows actually updated (0 when the edge id is
    /// unknown) so callers can report an accurate `changed` count instead of
    /// overstating (NEW-M1).
    async fn update_edge_target(&self, edge_id: i64, new_target_id: i64) -> Result<usize>;

    // ── evidence + links ──────────────────────────────────────
    async fn create_evidence(&self, e: &Evidence) -> Result<i64>;
    /// Idempotent: inserting a duplicate (source, evidence) pair is a no-op.
    async fn link_evidence(
        &self,
        source_type: EvidenceSourceType,
        source_id: i64,
        evidence_id: i64,
    ) -> Result<()>;
    async fn get_evidence_for(
        &self,
        source_type: EvidenceSourceType,
        source_id: i64,
    ) -> Result<Vec<Evidence>>;
    /// Fetch evidence linked to any of the given source ids (one query instead
    /// of N). Results are ordered by evidence id; empty input returns an empty
    /// vec without touching the DB.
    async fn get_evidence_for_many(
        &self,
        source_type: EvidenceSourceType,
        source_ids: &[i64],
    ) -> Result<Vec<Evidence>>;

    // ── mentions ─────────────────────────────────────────────
    async fn create_mention(&self, m: &Mention) -> Result<i64>;
    async fn get_mentions_for_object(&self, object_id: i64) -> Result<Vec<Mention>>;

    // ── compiler runs ────────────────────────────────────────
    async fn create_run(&self, r: &CompilerRun) -> Result<i64>;
    async fn finish_run(&self, id: i64, status: &str, statistics: &serde_json::Value)
    -> Result<()>;

    // ── traversal (memory export/import) ──────────────────────
    /// List every document (used to snapshot the whole graph for export).
    async fn list_documents(&self) -> Result<Vec<Document>>;
    /// List all objects belonging to a document.
    async fn list_objects_by_document(&self, doc_id: i64) -> Result<Vec<KnowledgeObject>>;
    /// List all edges belonging to a document (edges whose source object
    /// lives in that document).
    async fn list_edges_by_document(&self, doc_id: i64) -> Result<Vec<KnowledgeEdge>>;
    /// List all evidence rows belonging to a document.
    async fn list_evidence_by_document(&self, doc_id: i64) -> Result<Vec<Evidence>>;
    /// List every (source_type, source_id, evidence_id) link, for re-linking
    /// evidence during import.
    async fn list_evidence_links(&self) -> Result<Vec<KnowledgeEvidenceLink>>;
    /// Structured object search: filter by optional name substring, object
    /// type, property substring, and document scope. An absent filter is a
    /// wildcard; results are ordered by id (stable) and capped at `limit`.
    async fn search_objects(
        &self,
        name_contains: Option<&str>,
        object_type: Option<&str>,
        property_contains: Option<&str>,
        doc_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<KnowledgeObject>>;
    /// Aggregate row counts across the knowledge graph, for memory-health
    /// reporting (one lightweight query per table).
    async fn graph_counts(&self) -> Result<GraphCounts>;
    /// Find a V7 world entity by exact name.
    async fn find_world_entity(&self, name: &str) -> Result<Option<WorldEntity>>;
    /// Insert a V7 world entity, or return the existing id when a same-named
    /// entity already exists (upsert by `name`). Returns the entity id.
    async fn upsert_world_entity(
        &self,
        name: &str,
        entity_type: &str,
        importance: f64,
    ) -> Result<i64>;
    /// Insert a V7 entity profile key/value, or update the value when the key
    /// already exists for this entity (upsert by `(entity_id, key)`).
    async fn upsert_world_profile(
        &self,
        entity_id: i64,
        key: &str,
        value: &str,
        confidence: f64,
    ) -> Result<()>;
    /// Insert a V7 relation, deduplicating by `(source_id, target_id,
    /// relation_type)` — re-inserting the same edge is a no-op.
    async fn upsert_world_relation(
        &self,
        source_id: i64,
        target_id: i64,
        relation_type: &str,
        confidence: f64,
    ) -> Result<()>;
    /// List all V7 world entities (id, name, type, importance), for export.
    async fn list_world_entities(&self) -> Result<Vec<WorldEntity>>;
    /// List all V7 entity profiles, for export.
    async fn list_world_profiles(&self) -> Result<Vec<WorldProfile>>;
    /// List all V7 world relations, for export.
    async fn list_world_relations(&self) -> Result<Vec<WorldRelation>>;

    // ── high-level queries (dev_guide §5) ─────────────────────
    async fn inspect_entity(
        &self,
        name: &str,
        doc_title: Option<&str>,
    ) -> Result<Option<InspectEntityResult>>;
    async fn entity_timeline(
        &self,
        name: &str,
        doc_title: Option<&str>,
    ) -> Result<Vec<TimelineEntry>>;
    async fn relation_graph(
        &self,
        name: &str,
        depth: usize,
        doc_title: Option<&str>,
    ) -> Result<Option<RelationGraphResult>>;
    async fn search_evidence(
        &self,
        query: &str,
        doc_title: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EvidenceHit>>;
}
