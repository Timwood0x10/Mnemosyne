//! One-time migration: V1 domain model → general knowledge model.
//!
//! Mnemosyne's V1 stored characters/events/relations in dedicated domain
//! tables (`character_*`). The frozen general model (dev_guide §3) replaces
//! them with `knowledge_objects` / `knowledge_edges` / `evidence` / ...
//! Per the migration strategy (dev_guide §6 "不双写"): V1 stays as a legacy
//! read view, and this migrator copies V1 data *plus* the corpus text (for
//! documents / chapters / evidence content + offsets) into the general
//! tables — with **no functional discount** to the V1 dimensional scoring.
//!
//! Inputs:
//! - V1 `character_*` tables (via [`crate::character::CharacterStore`]) —
//!   objects / edges metadata.
//! - Corpus text files (via [`crate::ingest::corpus`]) — documents / chapters
//!   and the original-text snippets used as evidence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;

use crate::character::{CharacterAttribute, CharacterEvent, CharacterRelation, CharacterStore};
use crate::error::Result;
use crate::ingest::corpus;
use crate::ingest::extract;

use super::store::KnowledgeStore;
use super::{
    Chapter, CompilerRun, Document, Evidence, EvidenceSourceType, KnowledgeEdge, KnowledgeObject,
    Mention, ObjectType, Origin, SQLiteKnowledgeStore,
};

/// Tenant used by the V1 ingestion pipeline (see `src/ingest/mod.rs`).
const TENANT: &str = "novels";

/// Lore-compiler version stamp recorded against each migrated document.
const MIGRATOR_VERSION: &str = "lore-compiler v0.1.0 (v1-migration)";

/// The four classical novels the V1 pipeline ingests.
const NOVELS: &[&str] = &["水浒传", "三国演义", "红楼梦", "西游记"];

/// Aggregate counts produced by a migration run.
#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct MigrationStats {
    pub documents: usize,
    pub chapters: usize,
    pub objects: usize,
    pub edges: usize,
    pub evidence: usize,
    pub mentions: usize,
}

/// V1 source records preloaded BEFORE the write transaction opens.
///
/// Both the integration helper `ensure_sanguo_db` and the production CLI point
/// the V1 store and the knowledge store at the SAME SQLite file. Reading V1
/// through its own connection while the knowledge connection holds the H6
/// write transaction escalates the file lock, and the V1 reads then fail with
/// "database is locked". Snapshotting every V1 record up front keeps the
/// migration's H6 atomicity (one transaction, rollback on failure) while never
/// touching the V1 connection inside the transaction.
#[derive(Debug, Default)]
struct V1Snapshot {
    /// novel name → characters.
    characters: HashMap<String, Vec<CharacterAttribute>>,
    /// novel name → (character name → events).
    events: HashMap<String, HashMap<String, Vec<CharacterEvent>>>,
    /// novel name → (character name → relations).
    relations: HashMap<String, HashMap<String, Vec<CharacterRelation>>>,
}

/// Copies V1 domain data + corpus text into the general knowledge model.
///
/// Construct with [`Migrator::new`] then call [`Migrator::migrate`]. The
/// migrator only *reads* V1 and *writes* the general model; V1 tables are
/// left intact (legacy read view).
pub struct Migrator<'a> {
    v1: &'a dyn CharacterStore,
    knowledge: &'a SQLiteKnowledgeStore,
    corpus_dir: PathBuf,
}

impl<'a> Migrator<'a> {
    /// Create a new migrator pointing at a V1 store, a (fresh or existing)
    /// general store, and the corpus directory backing both.
    pub fn new(
        v1: &'a dyn CharacterStore,
        knowledge: &'a SQLiteKnowledgeStore,
        corpus_dir: &Path,
    ) -> Self {
        Self {
            v1,
            knowledge,
            corpus_dir: corpus_dir.to_path_buf(),
        }
    }

    /// Run the full V1 → general migration across all four novels.
    ///
    /// Novels whose corpus file is absent or empty are skipped (the V1
    /// pipeline does the same). A novel with a corpus but no V1 rows still
    /// produces documents + chapters (zero objects/edges).
    ///
    /// Idempotency: each `migrate_novel` call wipes that novel's prior
    /// knowledge rows (chapters/objects/edges/evidence/mentions) before
    /// re-inserting, so a re-run is a clean per-novel rebuild rather than an
    /// accumulating append. The V1 character store is never touched.
    pub async fn migrate(&self) -> Result<MigrationStats> {
        // Snapshot the V1 source BEFORE opening the write transaction.
        // `ensure_sanguo_db` and production both point v1 and knowledge at the
        // SAME SQLite file: reading through the v1 connection while the
        // knowledge connection holds the write transaction escalates the lock
        // to PENDING and v1's reads fail with "database is locked" (a big
        // chapter/object insert triggers the escalation). Reading all V1 data
        // up front, outside the transaction, keeps the H6 atomicity (one
        // transaction, roll back on failure) while never touching v1 inside it.
        let snapshot = self.load_v1_snapshot().await?;

        // Caller-owned transaction: if the caller already opened one on the
        // knowledge connection, we must NOT nest a BEGIN/COMMIT pair — SQLite
        // rejects the nested BEGIN, and a failed begin would corrupt the
        // caller's transaction. Run the inner work directly inside the
        // caller's transaction (the caller owns atomicity and FK policy).
        if self.knowledge.in_transaction().await? {
            return self.migrate_inner(&snapshot).await;
        }

        // Self-owned transaction: the original H6 path. Disable FK enforcement
        // for the duration of the migration: the migrator inserts in
        // parent→child order so enforcement is unnecessary, and cross-novel
        // edges can transiently reference not-yet-migrated objects. Re-enable
        // unconditionally afterwards so production queries keep FK integrity
        // checking. This PRAGMA must run OUTSIDE the transaction below —
        // SQLite ignores foreign_keys changes mid-transaction.
        self.knowledge.set_foreign_keys_enabled(false).await?;
        self.knowledge.begin_transaction().await?;
        let result = self.migrate_inner(&snapshot).await;
        match &result {
            Ok(_) => {
                // Best-effort re-enable FKs first (outside the transaction);
                // if commit then fails we still surface it.
                let _ = self.knowledge.set_foreign_keys_enabled(true).await;
                self.knowledge.commit_transaction().await?;
            }
            Err(_) => {
                // Discard partial writes, then restore FK enforcement.
                let _ = self.knowledge.rollback_transaction().await;
                let _ = self.knowledge.set_foreign_keys_enabled(true).await;
            }
        }
        result
    }

    async fn migrate_inner(&self, snapshot: &V1Snapshot) -> Result<MigrationStats> {
        let mut stats = MigrationStats::default();
        for novel in NOVELS {
            let s = self.migrate_novel(novel, snapshot).await?;
            stats.documents += s.documents;
            stats.chapters += s.chapters;
            stats.objects += s.objects;
            stats.edges += s.edges;
            stats.evidence += s.evidence;
            stats.mentions += s.mentions;
        }
        Ok(stats)
    }

    /// Preload every V1 record the migration reads, BEFORE the write
    /// transaction opens.
    ///
    /// `ensure_sanguo_db` (and the production CLI) point `v1` and `knowledge`
    /// at the SAME SQLite file. Reading through the `v1` connection while the
    /// `knowledge` connection holds the H6 write transaction escalates the
    /// file lock to PENDING, and `v1`'s reads then fail with "database is
    /// locked" (a big chapter/object insert triggers the escalation). Loading
    /// all V1 data up front keeps the H6 atomicity (one transaction, rollback
    /// on failure) while never touching `v1` inside it.
    async fn load_v1_snapshot(&self) -> Result<V1Snapshot> {
        let mut snapshot = V1Snapshot::default();
        for novel in NOVELS {
            let characters = self
                .v1
                .search_characters("", TENANT, Some(novel), 100_000)
                .await?;
            let mut events: HashMap<String, Vec<CharacterEvent>> = HashMap::new();
            let mut relations: HashMap<String, Vec<CharacterRelation>> = HashMap::new();
            for c in &characters {
                events.insert(
                    c.name.clone(),
                    self.v1
                        .get_character_events(&c.name, TENANT, Some(novel))
                        .await?,
                );
                relations.insert(
                    c.name.clone(),
                    self.v1
                        .get_relations_for_character(&c.name, TENANT, Some(novel))
                        .await?,
                );
            }
            snapshot.characters.insert(novel.to_string(), characters);
            snapshot.events.insert(novel.to_string(), events);
            snapshot.relations.insert(novel.to_string(), relations);
        }
        Ok(snapshot)
    }

    /// Migrate a single novel.
    async fn migrate_novel(&self, novel: &str, snapshot: &V1Snapshot) -> Result<MigrationStats> {
        let mut stats = MigrationStats::default();

        // Load corpus chapters; skip novels whose file is absent or empty.
        let chapters = match corpus::load_novel(novel, &self.corpus_dir) {
            Ok(c) => c,
            Err(_) => return Ok(stats), // no corpus file → nothing to migrate
        };
        if chapters.is_empty() {
            return Ok(stats);
        }

        // 1. Document (idempotent: reuse if this novel was migrated before).
        //    When reusing an existing document, wipe its prior knowledge rows
        //    first so re-migrating doesn't duplicate chapters/objects/edges.
        let doc_id = match self.knowledge.find_document_by_title(novel).await? {
            Some(d) => {
                self.knowledge.clear_for_document(d.id).await?;
                d.id
            }
            None => {
                self.knowledge
                    .create_document(&Document {
                        id: 0,
                        title: novel.to_string(),
                        author: None,
                        doc_type: Some("novel".into()),
                        created_at: now_ts(),
                    })
                    .await?
            }
        };
        stats.documents += 1;

        // 2. Chapters with cumulative within-document byte offsets.
        let mut chapter_ids: HashMap<i32, i64> = HashMap::new();
        let mut cursor: i64 = 0;
        for ch in &chapters {
            let start = cursor;
            let end = cursor + ch.text.len() as i64;
            cursor = end;
            let cid = self
                .knowledge
                .create_chapter(&Chapter {
                    id: 0,
                    doc_id,
                    chapter_no: ch.num,
                    title: None,
                    content: ch.text.clone(),
                    start_offset: Some(start),
                    end_offset: Some(end),
                })
                .await?;
            chapter_ids.insert(ch.num, cid);
        }
        stats.chapters += chapters.len();

        // 3. Persons from V1 character_attributes (preloaded snapshot).
        let v1_chars = snapshot.characters.get(novel);
        let v1_chars: &[CharacterAttribute] = v1_chars.map(|v| v.as_slice()).unwrap_or(&[]);
        let mut person_ids: HashMap<String, i64> = HashMap::new();
        for c in v1_chars {
            let props = json!({
                "aliases": c.aliases,
                "clothing": c.clothing,
                "personality": c.personality,
                "description": c.description,
                "novel": c.novel,
                "importance": c.importance,
                "tenant_id": c.tenant_id,
            });
            let oid = self
                .knowledge
                .create_object(&KnowledgeObject {
                    id: 0,
                    doc_id,
                    object_type: ObjectType::Person,
                    name: c.name.clone(),
                    properties: props,
                    confidence: c.importance,
                    created_at: now_ts(),
                })
                .await?;
            person_ids.insert(c.name.clone(), oid);
            stats.objects += 1;
        }

        // 4. Events from V1 character_events → event objects + participated_in
        //    edges. Dedupe event objects by (name, chapter) so an event shared
        //    across multiple related characters is stored once.
        let mut event_obj_ids: HashMap<(String, i32), i64> = HashMap::new();
        for c in v1_chars {
            let events: &[CharacterEvent] = snapshot
                .events
                .get(novel)
                .and_then(|m| m.get(&c.name))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let person_id = match person_ids.get(&c.name) {
                Some(id) => *id,
                None => continue,
            };
            for ev in events {
                let key = (ev.event_name.clone(), ev.chapter);
                let event_id = match event_obj_ids.get(&key) {
                    Some(id) => *id,
                    None => {
                        let props = json!({
                            "description": ev.description,
                            "chapter": ev.chapter,
                            "related_characters": ev.related_characters,
                            "importance": ev.importance,
                            "novel": ev.novel,
                            "character_name": ev.character_name,
                        });
                        let id = self
                            .knowledge
                            .create_object(&KnowledgeObject {
                                id: 0,
                                doc_id,
                                object_type: ObjectType::Event,
                                name: ev.event_name.clone(),
                                properties: props,
                                confidence: ev.importance,
                                created_at: now_ts(),
                            })
                            .await?;
                        event_obj_ids.insert(key, id);
                        stats.objects += 1;
                        id
                    }
                };

                let edge_id = self
                    .knowledge
                    .create_edge(&KnowledgeEdge {
                        id: 0,
                        source_id: person_id,
                        target_id: event_id,
                        predicate: "participated_in".into(),
                        properties: json!({"importance": ev.importance}),
                        origin: Origin::Observed,
                        confidence: ev.importance,
                        valid_from: Some(ev.chapter),
                        valid_to: None,
                        created_at: now_ts(),
                    })
                    .await?;
                stats.edges += 1;

                // Evidence: the event description, located at its chapter. If
                // the chapter wasn't parsed from the corpus (e.g. stale V1
                // data), skip evidence creation rather than storing a dangling
                // chapter_id=0 — such a row would be invisible to
                // `search_evidence` (INNER JOIN on chapters) yet visible to
                // `inspect_entity`, an inconsistent view. The edge above is
                // already created, so the relation itself is preserved.
                let chapter_id = match chapter_ids.get(&ev.chapter).copied() {
                    Some(id) => id,
                    None => continue,
                };
                let eid = self
                    .knowledge
                    .create_evidence(&Evidence {
                        id: 0,
                        doc_id,
                        chapter_id,
                        start_offset: None,
                        end_offset: None,
                        content: ev.description.clone(),
                        created_at: now_ts(),
                    })
                    .await?;
                self.knowledge
                    .link_evidence(EvidenceSourceType::Object, event_id, eid)
                    .await?;
                self.knowledge
                    .link_evidence(EvidenceSourceType::Edge, edge_id, eid)
                    .await?;
                self.knowledge
                    .link_evidence(EvidenceSourceType::Object, person_id, eid)
                    .await?;
                stats.evidence += 1;
            }
        }

        // 5. Relations from V1 character_relations → person↔person edges.
        //    Relations are stored once in V1 (bidirections=true) but are
        //    visible from both endpoints, so dedupe by V1 relation id.
        let mut seen_relation_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for c in v1_chars {
            let rels: &[CharacterRelation] = snapshot
                .relations
                .get(novel)
                .and_then(|m| m.get(&c.name))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            for r in rels {
                if !seen_relation_ids.insert(r.id.clone()) {
                    continue;
                }
                let (source_id, target_id) = match (
                    person_ids.get(&r.source_character),
                    person_ids.get(&r.target_character),
                ) {
                    (Some(s), Some(t)) => (*s, *t),
                    _ => continue, // relation to an un-migrated character
                };
                // Preserve the V1 dimensional scoring verbatim (no discount).
                let edge_id = self
                    .knowledge
                    .create_edge(&KnowledgeEdge {
                        id: 0,
                        source_id,
                        target_id,
                        predicate: r.relation_type.clone(),
                        properties: json!({
                            "novel": r.novel,
                            "importance": r.importance,
                            "confidence": r.confidence,
                            "bidirections": r.bidirections,
                            "source_type": r.source_type.as_str(),
                            "description": r.description,
                            "dimensional_scores": r.metadata.entries,
                        }),
                        origin: Origin::Observed,
                        confidence: r.confidence,
                        valid_from: Some(r.chapter),
                        valid_to: None,
                        created_at: now_ts(),
                    })
                    .await?;
                stats.edges += 1;

                // Skip evidence if the relation's chapter wasn't parsed from
                // the corpus (avoids a dangling chapter_id=0; see events above).
                let chapter_id = match chapter_ids.get(&r.chapter).copied() {
                    Some(id) => id,
                    None => continue,
                };
                let eid = self
                    .knowledge
                    .create_evidence(&Evidence {
                        id: 0,
                        doc_id,
                        chapter_id,
                        start_offset: None,
                        end_offset: None,
                        content: r.description.clone(),
                        created_at: now_ts(),
                    })
                    .await?;
                self.knowledge
                    .link_evidence(EvidenceSourceType::Edge, edge_id, eid)
                    .await?;
                stats.evidence += 1;
            }
        }

        // 6. Mentions + per-chapter text evidence: for each person, find the
        //    first occurrence (canonical name or any alias, longest-first) in
        //    each chapter. ≤1 mention+evidence per (person, chapter) bounds
        //    the volume while still giving `inspect_entity` real traceable
        //    evidence for every chapter a character appears in.
        for c in v1_chars {
            let person_id = match person_ids.get(&c.name) {
                Some(id) => *id,
                None => continue,
            };
            // Search names: canonical first, then aliases, longest-first so a
            // multi-char name wins over a single-char alias that happens to be
            // a substring of it.
            let mut search_names: Vec<String> = std::iter::once(c.name.clone())
                .chain(c.aliases.iter().cloned())
                .filter(|s| !s.is_empty())
                .collect();
            search_names.sort_by_key(|s| std::cmp::Reverse(s.len()));

            for ch in &chapters {
                let chapter_id = match chapter_ids.get(&ch.num) {
                    Some(id) => *id,
                    None => continue,
                };
                if let Some((alias, start, end)) = first_match(&ch.text, &search_names) {
                    // Slice a ~120-char window around the match, clamped to
                    // char boundaries so multi-byte Chinese never panics.
                    let lo = extract::floor_char_boundary(&ch.text, start.saturating_sub(40));
                    let hi = extract::floor_char_boundary(
                        &ch.text,
                        std::cmp::min(ch.text.len(), end + 80),
                    );
                    let snippet = ch.text[lo..hi].to_string();

                    self.knowledge
                        .create_mention(&Mention {
                            id: 0,
                            object_id: person_id,
                            chapter_id,
                            start_offset: Some(start as i64),
                            end_offset: Some(end as i64),
                            alias_used: Some(alias),
                            confidence: 1.0,
                        })
                        .await?;
                    stats.mentions += 1;

                    let eid = self
                        .knowledge
                        .create_evidence(&Evidence {
                            id: 0,
                            doc_id,
                            chapter_id,
                            // Offsets must describe the stored `content` (the
                            // `lo..hi` snippet window), not the narrower alias
                            // match range `[start, end]` — otherwise a consumer
                            // slicing the chapter at these offsets gets just the
                            // alias (e.g. "赵云") instead of the evidence text.
                            start_offset: Some(lo as i64),
                            end_offset: Some(hi as i64),
                            content: snippet,
                            created_at: now_ts(),
                        })
                        .await?;
                    self.knowledge
                        .link_evidence(EvidenceSourceType::Object, person_id, eid)
                        .await?;
                    stats.evidence += 1;
                }
            }
        }

        // 7. Record a compiler_run so the build is traceable & comparable.
        let run_id = self
            .knowledge
            .create_run(&CompilerRun {
                id: 0,
                doc_id,
                version: MIGRATOR_VERSION.into(),
                started_at: Some(now_ts()),
                finished_at: None,
                status: Some("running".into()),
                statistics: None,
            })
            .await?;
        self.knowledge
            .finish_run(
                run_id,
                "completed",
                &json!({
                    "objects": stats.objects,
                    "edges": stats.edges,
                    "evidence": stats.evidence,
                    "mentions": stats.mentions,
                    "chapters": stats.chapters,
                }),
            )
            .await?;

        Ok(stats)
    }
}

/// Find the first occurrence in `text` of any of `names` (longest-first
/// preferred via the caller's sort). Returns `(alias, start, end)` on the
/// earliest match, or `None`.
///
/// Overlap is resolved by earliest start position; ties broken by longest
/// name (caller sorts longest-first so the first found at a position wins).
/// Only the first match per name is considered — we want the earliest start
/// overall, not every occurrence of every name.
fn first_match(text: &str, names: &[String]) -> Option<(String, usize, usize)> {
    let mut best: Option<(usize, usize, &str)> = None;
    for n in names {
        // Take only the first match position for this name: a later
        // occurrence of the same name cannot beat the earliest start across
        // all names, and a different name may yet match earlier.
        if let Some((pos, _)) = text.match_indices(n.as_str()).next() {
            let end = pos + n.len();
            match best {
                None => best = Some((pos, end, n.as_str())),
                Some((bp, _, _)) if pos < bp => best = Some((pos, end, n.as_str())),
                _ => {}
            }
        }
    }
    best.map(|(start, end, alias)| (alias.to_string(), start, end))
}

/// Current unix timestamp (seconds).
fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::{
        CharacterAttribute, CharacterEvent, CharacterRelation, RelationSource, SQLiteCharacterStore,
    };
    use crate::types::Metadata;
    use chrono::Utc;
    use uuid::Uuid;

    /// Write a tiny two-chapter 三国演义 corpus into `dir`.
    fn write_tiny_corpus(dir: &Path) {
        let content = "\
第一回 宴桃园豪杰三结义
刘备、关羽、张飞三人结义为兄弟，誓同生死。
第二回 赵云救主
赵云单骑救阿斗，杀透重围。
";
        std::fs::write(dir.join("三国演义.txt"), content).expect("write tiny corpus");
        // Empty files for the other novels so load_novel skips them.
        for empty in &["水浒传.txt", "红楼梦.txt", "西游记.txt"] {
            std::fs::write(dir.join(empty), "").expect("write empty novel");
        }
    }

    fn sample_char(name: &str) -> CharacterAttribute {
        CharacterAttribute {
            id: Uuid::new_v4().to_string(),
            tenant_id: TENANT.into(),
            name: name.into(),
            novel: "三国演义".into(),
            aliases: vec![],
            clothing: String::new(),
            personality: String::new(),
            description: "登场三国演义".to_string(),
            importance: 0.5,
            created_at: Utc::now(),
            metadata: Metadata::default(),
        }
    }

    async fn seed_v1(store: &SQLiteCharacterStore) {
        for name in &["刘备", "关羽", "赵云"] {
            store.create_character(&sample_char(name)).await.unwrap();
        }
        // One event for 赵云 at chapter 2.
        store
            .create_event(&CharacterEvent {
                id: Uuid::new_v4().to_string(),
                tenant_id: TENANT.into(),
                character_name: "赵云".into(),
                event_name: "第2回 赵云单骑救主".into(),
                description: "赵云单骑救阿斗".into(),
                chapter: 2,
                novel: "三国演义".into(),
                related_characters: vec!["阿斗".into()],
                importance: 0.9,
                created_at: Utc::now(),
                metadata: Metadata::default(),
            })
            .await
            .unwrap();
        // One relation 刘备↔关羽 结义 at chapter 1.
        store
            .create_relation(&CharacterRelation {
                id: Uuid::new_v4().to_string(),
                tenant_id: TENANT.into(),
                source_character: "刘备".into(),
                target_character: "关羽".into(),
                relation_type: "结义".into(),
                description: "桃园结义".into(),
                chapter: 1,
                novel: "三国演义".into(),
                bidirections: true,
                source_type: RelationSource::CoOccurrence,
                confidence: 0.9,
                importance: 0.9,
                created_at: Utc::now(),
                metadata: Metadata::default(),
            })
            .await
            .unwrap();
    }

    /// Objective: Verify the migrator copies V1 characters/events/relations
    /// into the general model and that `inspect_entity` works post-migration.
    /// Invariants:
    /// - 1 document, 2 chapters, >= 3 person objects + 1 event object.
    /// - >= 2 edges (1 participated_in + 1 结义 relation).
    /// - mentions + evidence > 0 (corpus mentions the characters).
    /// - `inspect_entity("赵云")` returns the person + a participated_in event.
    #[tokio::test]
    async fn migrator_copies_v1_into_general_model() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_tiny_corpus(tmp.path());

        let v1 = SQLiteCharacterStore::open_in_memory()
            .await
            .expect("v1 open");
        seed_v1(&v1).await;
        let knowledge = SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("k open");

        let migrator = Migrator::new(&v1, &knowledge, tmp.path());
        let stats = migrator.migrate().await.expect("migrate");

        assert_eq!(stats.documents, 1, "one novel migrated");
        assert_eq!(stats.chapters, 2, "two chapters");
        assert!(
            stats.objects >= 4,
            ">= 3 persons + 1 event, got {}",
            stats.objects
        );
        assert!(
            stats.edges >= 2,
            ">= 1 participated_in + 1 relation, got {}",
            stats.edges
        );
        assert!(stats.mentions > 0, "corpus mentions characters");
        assert!(stats.evidence > 0, "evidence created");

        let zhaoyun = knowledge
            .inspect_entity("赵云", Some("三国演义"))
            .await
            .expect("inspect")
            .expect("赵云 found");
        assert_eq!(zhaoyun.object.name, "赵云");
        assert!(
            !zhaoyun.events.is_empty(),
            "赵云 should have a participated_in event"
        );
        assert!(
            zhaoyun.evidences.iter().any(|e| e.content.contains("阿斗")),
            "evidence should reference the rescue text"
        );
        assert!(
            !zhaoyun.mentions.is_empty(),
            "赵云 should be mentioned in the corpus"
        );

        // V1 dimensional scoring preserved on the 结义 edge.
        let liubei = knowledge
            .inspect_entity("刘备", Some("三国演义"))
            .await
            .expect("inspect")
            .expect("刘备 found");
        assert!(
            liubei.relations.iter().any(|r| r.predicate == "结义"),
            "刘备 should retain the 结义 relation"
        );
    }

    /// Objective: Verify `first_match` prefers the earliest occurrence and
    /// resolves names by position, returning None on no match.
    /// Invariants: earliest start wins; empty input returns None.
    #[test]
    fn first_match_picks_earliest() {
        let text = "刘备与关羽及赵云";
        let names = vec!["赵云".to_string(), "刘备".to_string()];
        let m = first_match(text, &names).expect("match found");
        assert_eq!(m.0, "刘备");
        assert_eq!(m.1, 0);
        assert_eq!(m.2, "刘备".len());

        assert!(first_match("空文本", &["不存在".to_string()]).is_none());
    }

    /// Objective: Verify migration is idempotent when re-run on the same
    /// stores (document reuse via find_document_by_title; no panic).
    /// Invariants: a second migrate() call completes without error.
    #[tokio::test]
    async fn migrator_re_run_is_safe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_tiny_corpus(tmp.path());
        let v1 = SQLiteCharacterStore::open_in_memory().await.expect("v1");
        seed_v1(&v1).await;
        let knowledge = SQLiteKnowledgeStore::open_in_memory().await.expect("k");

        let migrator = Migrator::new(&v1, &knowledge, tmp.path());
        let _ = migrator.migrate().await.expect("first migrate");
        // Re-run must not error (documents reused, chapters appended).
        let _ = migrator.migrate().await.expect("second migrate is safe");
    }

    /// Objective: Verify migration runs inside a CALLER-owned transaction
    /// without nesting a BEGIN (SQLite rejects nested BEGIN; a failed begin
    /// would corrupt the caller's transaction). This is the "transaction is
    /// caller-level, not connection-level" fix.
    /// Invariants: after begin_transaction → migrate → commit, the migrated
    /// rows are visible; no error surfaced from the nested migrate.
    #[tokio::test]
    async fn migrator_runs_inside_caller_transaction() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_tiny_corpus(tmp.path());
        let v1 = SQLiteCharacterStore::open_in_memory().await.expect("v1");
        seed_v1(&v1).await;
        let knowledge = SQLiteKnowledgeStore::open_in_memory().await.expect("k");

        // Caller opens a transaction first.
        knowledge.begin_transaction().await.expect("caller begin");

        let migrator = Migrator::new(&v1, &knowledge, tmp.path());
        let stats = migrator.migrate().await.expect("migrate inside caller txn");

        // The migration must have actually run (data written into the
        // caller's transaction).
        assert!(stats.documents >= 1, "migrated inside caller transaction");
        assert!(
            knowledge.in_transaction().await.expect("query txn state"),
            "caller transaction must still be open after nested migrate"
        );

        // Caller commits — migrated rows become visible.
        knowledge.commit_transaction().await.expect("caller commit");
        let zhaoyun = knowledge
            .find_object_by_name("赵云", None)
            .await
            .expect("query")
            .expect("赵云 migrated and committed");
        assert_eq!(zhaoyun.name, "赵云");
    }

    /// Objective: Verify migration does NOT create evidence with a dangling
    /// `chapter_id=0` when a V1 event references a chapter absent from the
    /// corpus (regression for `chapter_id.unwrap_or(0)`).
    /// Invariants:
    /// - 赵云 has an event at chapter 99, which the 2-chapter corpus does NOT
    ///   contain.
    /// - The participated_in edge for that event still exists (relation
    ///   preserved — only evidence is skipped).
    /// - No evidence linked to 赵云 carries `chapter_id == 0` (the fix skips
    ///   evidence creation instead of storing a dangling reference that would
    ///   be invisible to `search_evidence` but visible to `inspect_entity`).
    #[tokio::test]
    async fn migrator_skips_evidence_when_chapter_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_tiny_corpus(tmp.path());

        let v1 = SQLiteCharacterStore::open_in_memory()
            .await
            .expect("v1 open");
        seed_v1(&v1).await;
        // Extra event at chapter 99 — NOT in the 2-chapter corpus.
        v1.create_event(&CharacterEvent {
            id: Uuid::new_v4().to_string(),
            tenant_id: TENANT.into(),
            character_name: "赵云".into(),
            event_name: "第99回 赵云后期".into(),
            description: "赵云后期事迹".into(),
            chapter: 99,
            novel: "三国演义".into(),
            related_characters: vec![],
            importance: 0.5,
            created_at: Utc::now(),
            metadata: Metadata::default(),
        })
        .await
        .expect("seed chapter-99 event");

        let knowledge = SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("k open");
        let migrator = Migrator::new(&v1, &knowledge, tmp.path());
        migrator.migrate().await.expect("migrate");

        let zhaoyun = knowledge
            .inspect_entity("赵云", Some("三国演义"))
            .await
            .expect("inspect")
            .expect("赵云 found");
        // The chapter-99 event still produced a participated_in edge (the
        // relation is preserved; only its evidence was skipped).
        assert!(
            zhaoyun.events.iter().any(|e| e.name.contains("第99回")),
            "the chapter-99 event edge should still exist"
        );
        // No evidence linked to 赵云 may carry a dangling chapter_id=0.
        assert!(
            !zhaoyun.evidences.iter().any(|e| e.chapter_id == 0),
            "no evidence should carry a dangling chapter_id=0"
        );
    }
}
