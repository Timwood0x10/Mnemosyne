use std::collections::HashSet;
use std::sync::Arc;

use crate::error::{Error, Result, StorageError};
use crate::types::Metadata;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

static CHAR_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS character_attributes (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT NOT NULL,
    name        TEXT NOT NULL,
    novel       TEXT NOT NULL DEFAULT '',
    aliases     TEXT NOT NULL DEFAULT '[]',
    clothing    TEXT NOT NULL DEFAULT '',
    personality TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    importance  REAL NOT NULL DEFAULT 0.0,
    created_at  TEXT NOT NULL,
    metadata    TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_char_attr_name ON character_attributes(name);
CREATE INDEX IF NOT EXISTS idx_char_attr_novel ON character_attributes(novel);
CREATE INDEX IF NOT EXISTS idx_char_attr_tenant ON character_attributes(tenant_id);

CREATE TABLE IF NOT EXISTS character_events (
    id                 TEXT PRIMARY KEY,
    tenant_id          TEXT NOT NULL,
    character_name     TEXT NOT NULL,
    event_name         TEXT NOT NULL,
    description        TEXT NOT NULL DEFAULT '',
    chapter            INTEGER NOT NULL DEFAULT 0,
    novel              TEXT NOT NULL DEFAULT '',
    related_characters TEXT NOT NULL DEFAULT '[]',
    importance         REAL NOT NULL DEFAULT 0.0,
    created_at         TEXT NOT NULL,
    metadata           TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_char_events_char ON character_events(character_name);
CREATE INDEX IF NOT EXISTS idx_char_events_novel ON character_events(novel);
CREATE INDEX IF NOT EXISTS idx_char_events_tenant ON character_events(tenant_id);

CREATE TABLE IF NOT EXISTS character_relations (
    id                TEXT PRIMARY KEY,
    tenant_id         TEXT NOT NULL,
    source_character  TEXT NOT NULL,
    target_character  TEXT NOT NULL,
    relation_type     TEXT NOT NULL,
    description       TEXT NOT NULL DEFAULT '',
    chapter           INTEGER NOT NULL DEFAULT 0,
    novel             TEXT NOT NULL DEFAULT '',
    bidirections      INTEGER NOT NULL DEFAULT 0,
    importance        REAL NOT NULL DEFAULT 0.0,
    created_at        TEXT NOT NULL,
    relation_source   TEXT NOT NULL DEFAULT 'co_occurrence',
    confidence        REAL NOT NULL DEFAULT 0.5,
    metadata          TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_char_rel_src ON character_relations(source_character);
CREATE INDEX IF NOT EXISTS idx_char_rel_tgt ON character_relations(target_character);
CREATE INDEX IF NOT EXISTS idx_char_rel_novel ON character_relations(novel);
CREATE INDEX IF NOT EXISTS idx_char_rel_tenant ON character_relations(tenant_id);

";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterAttribute {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub novel: String,
    pub aliases: Vec<String>,
    pub clothing: String,
    pub personality: String,
    pub description: String,
    pub importance: f64,
    pub created_at: DateTime<Utc>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterEvent {
    pub id: String,
    pub tenant_id: String,
    pub character_name: String,
    pub event_name: String,
    pub description: String,
    pub chapter: i32,
    pub novel: String,
    pub related_characters: Vec<String>,
    pub importance: f64,
    pub created_at: DateTime<Utc>,
    pub metadata: Metadata,
}

/// Relation source type for character relationships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationSource {
    /// Relationship detected via co-occurrence (generic)
    CoOccurrence,
    /// Relationship extracted from dialog chain
    DialogChain,
    /// Relationship explicitly mentioned in events/text
    EventExplicit,
    /// Relationship from keyword pattern match
    KeywordMatch,
}

impl RelationSource {
    pub fn as_str(&self) -> &str {
        match self {
            RelationSource::CoOccurrence => "co_occurrence",
            RelationSource::DialogChain => "dialog_chain",
            RelationSource::EventExplicit => "event_explicit",
            RelationSource::KeywordMatch => "keyword_match",
        }
    }
}

impl std::str::FromStr for RelationSource {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "co_occurrence" => Ok(RelationSource::CoOccurrence),
            "dialog_chain" => Ok(RelationSource::DialogChain),
            "event_explicit" => Ok(RelationSource::EventExplicit),
            "keyword_match" => Ok(RelationSource::KeywordMatch),
            _ => Err(format!("unknown relation source: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRelation {
    pub id: String,
    pub tenant_id: String,
    pub source_character: String,
    pub target_character: String,
    pub relation_type: String,
    pub description: String,
    pub chapter: i32,
    pub novel: String,
    pub bidirections: bool,
    pub source_type: RelationSource,
    pub confidence: f64,
    pub importance: f64,
    pub created_at: DateTime<Utc>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterNetworkNode {
    pub character: CharacterAttribute,
    pub events: Vec<CharacterEvent>,
    pub relations: Vec<CharacterRelation>,
    pub connections: Vec<CharacterNetworkNode>,
}

fn parse_aliases(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn aliases_to_json(a: &[String]) -> String {
    serde_json::to_string(a).unwrap_or_else(|_| "[]".to_string())
}

fn parse_char_list(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn char_list_to_json(c: &[String]) -> String {
    serde_json::to_string(c).unwrap_or_else(|_| "[]".to_string())
}

fn row_to_character(row: &rusqlite::Row) -> rusqlite::Result<CharacterAttribute> {
    let created_at_str: String = row.get("created_at")?;
    let created_at: DateTime<Utc> = created_at_str.parse().unwrap_or_else(|_| Utc::now());
    let aliases_str: String = row.get("aliases").unwrap_or_default();
    let meta_str: String = row.get("metadata").unwrap_or_default();
    Ok(CharacterAttribute {
        id: row.get("id")?,
        tenant_id: row.get("tenant_id")?,
        name: row.get("name")?,
        novel: row.get("novel")?,
        aliases: parse_aliases(&aliases_str),
        clothing: row.get("clothing")?,
        personality: row.get("personality")?,
        description: row.get("description")?,
        importance: row.get("importance")?,
        created_at,
        metadata: serde_json::from_str(&meta_str).unwrap_or_default(),
    })
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<CharacterEvent> {
    let created_at_str: String = row.get("created_at")?;
    let created_at: DateTime<Utc> = created_at_str.parse().unwrap_or_else(|_| Utc::now());
    let rc_str: String = row.get("related_characters").unwrap_or_default();
    let meta_str: String = row.get("metadata").unwrap_or_default();
    Ok(CharacterEvent {
        id: row.get("id")?,
        tenant_id: row.get("tenant_id")?,
        character_name: row.get("character_name")?,
        event_name: row.get("event_name")?,
        description: row.get("description")?,
        chapter: row.get("chapter")?,
        novel: row.get("novel")?,
        related_characters: parse_char_list(&rc_str),
        importance: row.get("importance")?,
        created_at,
        metadata: serde_json::from_str(&meta_str).unwrap_or_default(),
    })
}

fn row_to_relation(row: &rusqlite::Row) -> rusqlite::Result<CharacterRelation> {
    let created_at_str: String = row.get("created_at")?;
    let created_at: DateTime<Utc> = created_at_str.parse().unwrap_or_else(|_| Utc::now());
    let meta_str: String = row.get("metadata").unwrap_or_default();
    Ok(CharacterRelation {
        id: row.get("id")?,
        tenant_id: row.get("tenant_id")?,
        source_character: row.get("source_character")?,
        target_character: row.get("target_character")?,
        relation_type: row.get("relation_type")?,
        description: row.get("description")?,
        chapter: row.get("chapter")?,
        novel: row.get("novel")?,
        bidirections: row.get("bidirections")?,
        source_type: {
            let s: String = row.get("relation_source")?;
            // Fall back to CoOccurrence on unknown DB values instead of
            // panicking — defensive against legacy or foreign rows.
            s.parse::<RelationSource>()
                .unwrap_or(RelationSource::CoOccurrence)
        },
        confidence: row.get("confidence")?,
        importance: row.get("importance")?,
        created_at,
        metadata: serde_json::from_str(&meta_str).unwrap_or_default(),
    })
}

#[async_trait]
pub trait CharacterStore: Send + Sync {
    async fn create_character(&self, c: &CharacterAttribute) -> Result<()>;
    async fn get_character(&self, id: &str) -> Result<Option<CharacterAttribute>>;
    async fn search_characters(
        &self,
        query: &str,
        tenant_id: &str,
        novel: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CharacterAttribute>>;
    async fn search_characters_by_name(
        &self,
        name: &str,
        tenant_id: &str,
        novel: Option<&str>,
    ) -> Result<Vec<CharacterAttribute>>;
    async fn update_character(&self, c: &CharacterAttribute) -> Result<()>;
    async fn delete_character(&self, id: &str) -> Result<()>;

    async fn create_event(&self, e: &CharacterEvent) -> Result<()>;
    async fn get_character_events(
        &self,
        character_name: &str,
        tenant_id: &str,
        novel: Option<&str>,
    ) -> Result<Vec<CharacterEvent>>;
    async fn delete_event(&self, id: &str) -> Result<()>;

    async fn create_relation(&self, r: &CharacterRelation) -> Result<()>;
    async fn get_relations_for_character(
        &self,
        character_name: &str,
        tenant_id: &str,
        novel: Option<&str>,
    ) -> Result<Vec<CharacterRelation>>;
    async fn delete_relation(&self, id: &str) -> Result<()>;

    async fn count_characters(&self, tenant_id: &str, novel: Option<&str>) -> Result<i64>;
    async fn count_events(&self, tenant_id: &str, novel: Option<&str>) -> Result<i64>;
    async fn count_relations(&self, tenant_id: &str, novel: Option<&str>) -> Result<i64>;
}

pub struct SQLiteCharacterStore {
    conn: Arc<Mutex<Connection>>,
}

impl SQLiteCharacterStore {
    pub async fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| StorageError::Schema(format!("open character store: {e}")))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init().await?;
        Ok(store)
    }

    pub async fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| StorageError::Schema(format!("open in-memory: {e}")))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        // busy_timeout makes concurrent connections wait (up to 5s) for a lock
        // instead of failing immediately. foreign_keys enforces declared FK
        // constraints. WAL is intentionally NOT enabled: the knowledge store
        // uses the same rollback-journal mode, and mixing WAL on one connection
        // with rollback journal on another connection to the SAME file leaves
        // `-wal`/`-shm` sidecars that the next process reads as "file is not a
        // database". Both stores must agree on the journal mode for a shared
        // DB file (e.g. integration-test fixtures).
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )?;
        conn.execute_batch(CHAR_SCHEMA)
            .map_err(|e| StorageError::Schema(format!("init character schema: {e}")))?;
        Ok(())
    }
}

mod store;

pub use store::traverse_character_network;

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn sample_char(name: &str, novel: &str) -> CharacterAttribute {
        CharacterAttribute {
            id: Uuid::new_v4().to_string(),
            tenant_id: "novels".to_string(),
            name: name.to_string(),
            novel: novel.to_string(),
            aliases: vec![],
            clothing: String::new(),
            personality: String::new(),
            description: String::new(),
            importance: 0.5,
            created_at: Utc::now(),
            metadata: Metadata::default(),
        }
    }

    fn sample_event(char_name: &str, event: &str, ch: i32) -> CharacterEvent {
        CharacterEvent {
            id: Uuid::new_v4().to_string(),
            tenant_id: "novels".to_string(),
            character_name: char_name.to_string(),
            event_name: event.to_string(),
            description: String::new(),
            chapter: ch,
            novel: "水浒传".to_string(),
            related_characters: vec![],
            importance: 0.5,
            created_at: Utc::now(),
            metadata: Metadata::default(),
        }
    }

    fn sample_relation(src: &str, tgt: &str, rtype: &str) -> CharacterRelation {
        CharacterRelation {
            id: Uuid::new_v4().to_string(),
            tenant_id: "novels".to_string(),
            source_character: src.to_string(),
            target_character: tgt.to_string(),
            relation_type: rtype.to_string(),
            description: String::new(),
            chapter: 0,
            novel: "水浒传".to_string(),
            bidirections: false,
            source_type: RelationSource::CoOccurrence,
            confidence: 0.5,
            importance: 0.5,
            created_at: Utc::now(),
            metadata: Metadata::default(),
        }
    }

    #[tokio::test]
    async fn create_and_get_character() {
        let store = SQLiteCharacterStore::open_in_memory().await.expect("open");
        let c = sample_char("扈三娘", "水浒传");
        store.create_character(&c).await.expect("create");
        let got = store
            .get_character(&c.id)
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(got.name, "扈三娘");
        assert_eq!(got.novel, "水浒传");
    }

    #[tokio::test]
    async fn search_by_name_exact() {
        let store = SQLiteCharacterStore::open_in_memory().await.expect("open");
        store
            .create_character(&sample_char("武松", "水浒传"))
            .await
            .expect("create");
        store
            .create_character(&sample_char("宋江", "水浒传"))
            .await
            .expect("create");
        let results = store
            .search_characters_by_name("武松", "novels", None)
            .await
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "武松");
    }

    #[tokio::test]
    async fn fts_search_works() {
        let store = SQLiteCharacterStore::open_in_memory().await.expect("open");
        let mut c = sample_char("扈三娘", "水浒传");
        c.clothing = "骑一匹青马，使两口日月双刀，戴金冠，披红袍".to_string();
        store.create_character(&c).await.expect("create");
        let results = store
            .search_characters("青马", "novels", None, 10)
            .await
            .expect("search");
        assert!(!results.is_empty(), "LIKE should find '青马'");
    }

    #[tokio::test]
    async fn create_and_query_events() {
        let store = SQLiteCharacterStore::open_in_memory().await.expect("open");
        let c = sample_char("扈三娘", "水浒传");
        store.create_character(&c).await.expect("create");
        store
            .create_event(&sample_event("扈三娘", "单捉王矮虎", 47))
            .await
            .expect("create");
        store
            .create_event(&sample_event("扈三娘", "被林冲活擒", 47))
            .await
            .expect("create");
        let events = store
            .get_character_events("扈三娘", "novels", None)
            .await
            .expect("get");
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn create_and_query_relations() {
        let store = SQLiteCharacterStore::open_in_memory().await.expect("open");
        store
            .create_character(&sample_char("扈三娘", "水浒传"))
            .await
            .expect("create");
        store
            .create_character(&sample_char("林冲", "水浒传"))
            .await
            .expect("create");
        store
            .create_relation(&sample_relation("扈三娘", "林冲", "被擒"))
            .await
            .expect("create");
        let rels = store
            .get_relations_for_character("扈三娘", "novels", None)
            .await
            .expect("get");
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relation_type, "被擒");
    }

    #[tokio::test]
    async fn delete_character() {
        let store = SQLiteCharacterStore::open_in_memory().await.expect("open");
        let c = sample_char("武松", "水浒传");
        store.create_character(&c).await.expect("create");
        store.delete_character(&c.id).await.expect("delete");
        assert!(store.get_character(&c.id).await.expect("get").is_none());
    }

    #[tokio::test]
    async fn count_aggregations() {
        let store = SQLiteCharacterStore::open_in_memory().await.expect("open");
        store
            .create_character(&sample_char("武松", "水浒传"))
            .await
            .expect("create");
        store
            .create_character(&sample_char("宋江", "水浒传"))
            .await
            .expect("create");
        store
            .create_character(&sample_char("林黛玉", "红楼梦"))
            .await
            .expect("create");
        assert_eq!(
            store
                .count_characters("novels", Some("水浒传"))
                .await
                .expect("count"),
            2
        );
        assert_eq!(
            store
                .count_characters("novels", Some("红楼梦"))
                .await
                .expect("count"),
            1
        );
        assert_eq!(
            store.count_characters("novels", None).await.expect("count"),
            3
        );
    }

    #[tokio::test]
    async fn network_traversal() {
        let store = SQLiteCharacterStore::open_in_memory().await.expect("open");
        for name in &["扈三娘", "林冲", "宋江", "王英"] {
            store
                .create_character(&sample_char(name, "水浒传"))
                .await
                .expect("create");
        }
        store
            .create_event(&{
                let mut e = sample_event("扈三娘", "被林冲活擒", 47);
                e.related_characters = vec!["林冲".to_string(), "宋江".to_string()];
                e
            })
            .await
            .expect("create");
        store
            .create_event(&sample_event("扈三娘", "单捉王矮虎", 47))
            .await
            .expect("create");
        store
            .create_event(&sample_event("林冲", "火并王伦", 19))
            .await
            .expect("create");
        store
            .create_relation(&sample_relation("扈三娘", "林冲", "被擒"))
            .await
            .expect("create");
        store
            .create_relation(&sample_relation("扈三娘", "王英", "夫妻"))
            .await
            .expect("create");

        let node = traverse_character_network(&store, "扈三娘", "novels", None, 2)
            .await
            .expect("traverse");
        assert_eq!(node.character.name, "扈三娘");
        assert_eq!(node.events.len(), 2);
        assert_eq!(node.relations.len(), 2);
        assert!(
            !node.connections.is_empty(),
            "should have connected characters"
        );
        let has_lin = node.connections.iter().any(|n| n.character.name == "林冲");
        assert!(has_lin, "should connect to 林冲");
    }
}
