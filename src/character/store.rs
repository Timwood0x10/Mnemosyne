//! `CharacterStore` trait implementation for the SQLite character store.

use super::*;
use async_trait::async_trait;

#[async_trait]
impl CharacterStore for SQLiteCharacterStore {
    async fn create_character(&self, c: &CharacterAttribute) -> Result<()> {
        let c = c.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO character_attributes (id, tenant_id, name, novel, aliases, clothing, personality, description, importance, created_at, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    c.id, c.tenant_id, c.name, c.novel,
                    crate::persistence::json::json_array(&c.aliases),
                    c.clothing, c.personality, c.description,
                    c.importance, c.created_at.to_rfc3339(),
                    crate::persistence::json::json_object(&c.metadata),
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn get_character(&self, id: &str) -> Result<Option<CharacterAttribute>> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare("SELECT * FROM character_attributes WHERE id = ?1")?;
            let mut rows = stmt.query_map(params![id], row_to_character)?;
            match rows.next() {
                Some(Ok(c)) => Ok(Some(c)),
                Some(Err(e)) => Err(StorageError::Sqlite(format!("get_character: {e}")).into()),
                None => Ok(None),
            }
        })
        .await
    }

    async fn search_characters(
        &self,
        query: &str,
        tenant_id: &str,
        novel: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CharacterAttribute>> {
        let query = query.to_owned();
        let tenant_id = tenant_id.to_owned();
        let novel = novel.map(str::to_owned);
        self.with_conn(move |conn| {
            // The LIKE escape order and the limit clamp are owned by
            // `crate::sql`, so this store cannot drift from the others again:
            // `09-27/M9` fixed exactly this copy after it escaped `%`/`_`
            // before `\` and dropped matches for backslash-terminated queries.
            let like = crate::sql::like_pattern(&query);
            let limit = crate::sql::sql_limit(limit);

            // One statement with an optional novel filter, spelled
            // `?3 IS NULL OR novel = ?3` — the shape `knowledge/store` already
            // uses. The two near-identical SQL strings this replaced differed
            // only in that clause and the placeholder numbering it shifted.
            let mut stmt = conn.prepare(
                "SELECT * FROM character_attributes \
                 WHERE (name LIKE ?1 ESCAPE '\\' \
                        OR aliases LIKE ?1 ESCAPE '\\' \
                        OR clothing LIKE ?1 ESCAPE '\\' \
                        OR personality LIKE ?1 ESCAPE '\\' \
                        OR description LIKE ?1 ESCAPE '\\') \
                   AND tenant_id = ?2 \
                   AND (?3 IS NULL OR novel = ?3) \
                 ORDER BY \
                   CASE WHEN name LIKE ?1 ESCAPE '\\' THEN 0 ELSE 1 END, \
                   importance DESC \
                 LIMIT ?4",
            )?;
            let rows = stmt.query_map(params![like, tenant_id, novel, limit], row_to_character)?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await
    }

    async fn search_characters_by_name(
        &self,
        name: &str,
        tenant_id: &str,
        novel: Option<&str>,
    ) -> Result<Vec<CharacterAttribute>> {
        let name = name.to_owned();
        let tenant_id = tenant_id.to_owned();
        let novel = novel.map(str::to_owned);
        self.with_conn(move |conn| {
            let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(n) =
                novel
            {
                (
                    "SELECT * FROM character_attributes WHERE name = ?1 AND tenant_id = ?2 AND novel = ?3 ORDER BY importance DESC".to_string(),
                    vec![
                        Box::new(name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(tenant_id.to_string()),
                        Box::new(n.to_string()),
                    ],
                )
            } else {
                (
                    "SELECT * FROM character_attributes WHERE (name = ?1 OR ?1 IN (SELECT value FROM json_each(aliases))) AND tenant_id = ?2 ORDER BY importance DESC".to_string(),
                    vec![
                        Box::new(name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(tenant_id.to_string()),
                    ],
                )
            };

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|p| p.as_ref())),
                row_to_character,
            )?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await
    }

    async fn update_character(&self, c: &CharacterAttribute) -> Result<()> {
        let c = c.clone();
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "UPDATE character_attributes SET name=?2, novel=?3, aliases=?4, clothing=?5, personality=?6, description=?7, importance=?8, metadata=?9 WHERE id=?1",
                params![
                    c.id, c.name, c.novel,
                    crate::persistence::json::json_array(&c.aliases),
                    c.clothing, c.personality, c.description,
                    c.importance,
                    crate::persistence::json::json_object(&c.metadata),
                ],
            )?;
            if affected == 0 {
                return Err(StorageError::NotFound(c.id.clone()).into());
            }
            Ok(())
        })
        .await
    }

    async fn delete_character(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "DELETE FROM character_attributes WHERE id = ?1",
                params![id],
            )?;
            if affected == 0 {
                return Err(StorageError::NotFound(id.to_string()).into());
            }
            Ok(())
        })
        .await
    }

    async fn create_event(&self, e: &CharacterEvent) -> Result<()> {
        let e = e.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO character_events (id, tenant_id, character_name, event_name, description, chapter, novel, related_characters, importance, created_at, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    e.id, e.tenant_id, e.character_name, e.event_name,
                    e.description, e.chapter, e.novel,
                    crate::persistence::json::json_array(&e.related_characters),
                    e.importance, e.created_at.to_rfc3339(),
                    crate::persistence::json::json_object(&e.metadata),
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn get_character_events(
        &self,
        character_name: &str,
        tenant_id: &str,
        novel: Option<&str>,
    ) -> Result<Vec<CharacterEvent>> {
        let character_name = character_name.to_owned();
        let tenant_id = tenant_id.to_owned();
        let novel = novel.map(str::to_owned);
        self.with_conn(move |conn| {
            let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(n) = novel {
                (
                    "SELECT * FROM character_events WHERE character_name = ?1 AND tenant_id = ?2 AND novel = ?3 ORDER BY chapter ASC".to_string(),
                    vec![
                        Box::new(character_name.to_string()),
                        Box::new(tenant_id.to_string()),
                        Box::new(n.to_string()),
                    ],
                )
            } else {
                (
                    "SELECT * FROM character_events WHERE character_name = ?1 AND tenant_id = ?2 ORDER BY chapter ASC".to_string(),
                    vec![
                        Box::new(character_name.to_string()),
                        Box::new(tenant_id.to_string()),
                    ],
                )
            };

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
                row_to_event,
            )?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await
    }

    async fn delete_event(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            let affected =
                conn.execute("DELETE FROM character_events WHERE id = ?1", params![id])?;
            if affected == 0 {
                return Err(StorageError::NotFound(id.to_string()).into());
            }
            Ok(())
        })
        .await
    }

    async fn create_relation(&self, r: &CharacterRelation) -> Result<()> {
        let r = r.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO character_relations (id, tenant_id, source_character, target_character, relation_type, description, chapter, novel, bidirections, importance, created_at, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    r.id, r.tenant_id, r.source_character, r.target_character,
                    r.relation_type, r.description, r.chapter, r.novel,
                    r.bidirections as i32, r.importance, r.created_at.to_rfc3339(),
                    crate::persistence::json::json_object(&r.metadata),
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn get_relations_for_character(
        &self,
        character_name: &str,
        tenant_id: &str,
        novel: Option<&str>,
    ) -> Result<Vec<CharacterRelation>> {
        let character_name = character_name.to_owned();
        let tenant_id = tenant_id.to_owned();
        let novel = novel.map(str::to_owned);
        self.with_conn(move |conn| {
            let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(n) =
                novel
            {
                (
                    "SELECT * FROM character_relations WHERE (source_character = ?1 OR target_character = ?1) AND tenant_id = ?2 AND novel = ?3 ORDER BY chapter ASC".to_string(),
                    vec![
                        Box::new(character_name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(tenant_id.to_string()),
                        Box::new(n.to_string()),
                    ],
                )
            } else {
                (
                    "SELECT * FROM character_relations WHERE (source_character = ?1 OR target_character = ?1) AND tenant_id = ?2 ORDER BY chapter ASC".to_string(),
                    vec![
                        Box::new(character_name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(tenant_id.to_string()),
                    ],
                )
            };

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|p| p.as_ref())),
                row_to_relation,
            )?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await
    }

    async fn delete_relation(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            let affected =
                conn.execute("DELETE FROM character_relations WHERE id = ?1", params![id])?;
            if affected == 0 {
                return Err(StorageError::NotFound(id.to_string()).into());
            }
            Ok(())
        })
        .await
    }

    async fn count_characters(&self, tenant_id: &str, novel: Option<&str>) -> Result<i64> {
        let tenant_id = tenant_id.to_owned();
        let novel = novel.map(str::to_owned);
        self.with_conn(move |conn| {
            if let Some(n) = novel {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM character_attributes WHERE tenant_id = ?1 AND novel = ?2",
                    params![tenant_id, n],
                    |row| row.get(0),
                )?)
            } else {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM character_attributes WHERE tenant_id = ?1",
                    params![tenant_id],
                    |row| row.get(0),
                )?)
            }
        })
        .await
    }

    async fn count_events(&self, tenant_id: &str, novel: Option<&str>) -> Result<i64> {
        let tenant_id = tenant_id.to_owned();
        let novel = novel.map(str::to_owned);
        self.with_conn(move |conn| {
            if let Some(n) = novel {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM character_events WHERE tenant_id = ?1 AND novel = ?2",
                    params![tenant_id, n],
                    |row| row.get(0),
                )?)
            } else {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM character_events WHERE tenant_id = ?1",
                    params![tenant_id],
                    |row| row.get(0),
                )?)
            }
        })
        .await
    }

    async fn count_relations(&self, tenant_id: &str, novel: Option<&str>) -> Result<i64> {
        let tenant_id = tenant_id.to_owned();
        let novel = novel.map(str::to_owned);
        self.with_conn(move |conn| {
            if let Some(n) = novel {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM character_relations WHERE tenant_id = ?1 AND novel = ?2",
                    params![tenant_id, n],
                    |row| row.get(0),
                )?)
            } else {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM character_relations WHERE tenant_id = ?1",
                    params![tenant_id],
                    |row| row.get(0),
                )?)
            }
        })
        .await
    }
}

pub async fn traverse_character_network(
    store: &dyn CharacterStore,
    name: &str,
    tenant_id: &str,
    novel: Option<&str>,
    max_depth: usize,
) -> Result<CharacterNetworkNode> {
    let chars = store
        .search_characters_by_name(name, tenant_id, novel)
        .await?;
    let character = chars
        .into_iter()
        .next()
        .ok_or_else(|| Error::InvalidInput(format!("character `{name}` not found")))?;

    let mut visited = HashSet::new();
    visited.insert(character.name.clone());
    for a in &character.aliases {
        visited.insert(a.clone());
    }

    let node =
        build_network_node(store, &character, tenant_id, novel, max_depth, &mut visited).await?;
    Ok(node)
}

async fn build_network_node(
    store: &dyn CharacterStore,
    character: &CharacterAttribute,
    tenant_id: &str,
    novel: Option<&str>,
    remaining_depth: usize,
    visited: &mut HashSet<String>,
) -> Result<CharacterNetworkNode> {
    let events = store
        .get_character_events(&character.name, tenant_id, novel)
        .await?;

    let relations = store
        .get_relations_for_character(&character.name, tenant_id, novel)
        .await?;

    let mut connections = Vec::new();
    if remaining_depth > 0 {
        let mut next_names: HashSet<String> = HashSet::new();
        for r in &relations {
            let other = if r.source_character == character.name {
                &r.target_character
            } else {
                &r.source_character
            };
            if !visited.contains(other.as_str()) {
                next_names.insert(other.clone());
            }
        }
        for ev in &events {
            for rc in &ev.related_characters {
                if !visited.contains(rc.as_str()) {
                    next_names.insert(rc.clone());
                }
            }
        }
        for next_name in next_names {
            visited.insert(next_name.clone());
            let chars = store
                .search_characters_by_name(&next_name, tenant_id, novel)
                .await?;
            if let Some(next_char) = chars.into_iter().next() {
                let child = Box::pin(build_network_node(
                    store,
                    &next_char,
                    tenant_id,
                    novel,
                    remaining_depth - 1,
                    visited,
                ))
                .await?;
                connections.push(child);
            }
        }
    }

    Ok(CharacterNetworkNode {
        character: character.clone(),
        events,
        relations,
        connections,
    })
}
