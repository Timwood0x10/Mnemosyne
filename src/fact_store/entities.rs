//! Entity identity resolution: tenant-scoped user, agent and entity lookup.

use super::*;

impl SqliteFactStore {
    /// Resolve or create a tenant-scoped entity with an optional external key.
    ///
    /// The `(tenant_id, external_key)` pair is the stable identity boundary for
    /// users. Non-user entities can omit `external_key` and remain name based.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the entity cannot be read or created.
    pub fn resolve_entity(
        &self,
        tenant_id: &str,
        external_key: Option<&str>,
        name: &str,
        entity_type: &str,
    ) -> Result<i64> {
        let conn = self.lock_conn()?;
        let existing = if let Some(key) = external_key {
            conn.query_row(
                "SELECT id FROM entities WHERE tenant_id = ?1 AND external_key = ?2",
                params![tenant_id, key],
                |row| row.get(0),
            )
            .optional()?
        } else {
            conn.query_row(
                "SELECT id FROM entities WHERE tenant_id = ?1 AND external_key IS NULL AND name = ?2 AND entity_type = ?3",
                params![tenant_id, name, entity_type],
                |row| row.get(0),
            )
            .optional()?
        };
        if let Some(id) = existing {
            return Ok(id);
        }

        if tenant_id == "default" && external_key == Some("default") && entity_type == "user" {
            let root = conn
                .query_row(
                    "SELECT name, entity_type, external_key FROM entities WHERE id = 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()?;
            match root {
                None => {
                    conn.execute(
                        "INSERT INTO entities (id, tenant_id, external_key, name, entity_type) VALUES (1, ?1, ?2, ?3, ?4)",
                        params![tenant_id, external_key, name, entity_type],
                    )?;
                    return Ok(1);
                }
                Some((root_name, root_type, None))
                    if root_name.eq_ignore_ascii_case("user") && root_type == "user" =>
                {
                    conn.execute(
                        "UPDATE entities SET tenant_id = ?1, external_key = ?2 WHERE id = 1",
                        params![tenant_id, external_key],
                    )?;
                    return Ok(1);
                }
                Some(_) => {}
            }
        }

        conn.execute(
            "INSERT INTO entities (tenant_id, external_key, name, entity_type) VALUES (?1, ?2, ?3, ?4)",
            params![tenant_id, external_key, name, entity_type],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Resolve or create a tenant-scoped user entity.
    ///
    /// Empty user ids map to `default` for backward compatibility. The returned
    /// id is stable for the same tenant/user pair and isolated from every other
    /// pair.
    ///
    /// # Errors
    ///
    /// Returns a storage error when identity resolution fails.
    pub fn resolve_user(&self, tenant_id: &str, user_id: &str) -> Result<i64> {
        let tenant_id = if tenant_id.trim().is_empty() {
            "default"
        } else {
            tenant_id.trim()
        };
        let user_id = if user_id.trim().is_empty() {
            "default"
        } else {
            user_id.trim()
        };
        let name = if user_id == "default" {
            "User".to_string()
        } else {
            format!("User:{user_id}")
        };
        self.resolve_entity(tenant_id, Some(user_id), &name, "user")
    }

    /// Resolve or create a tenant-scoped **Agent** entity, distinct from every
    /// User entity.
    ///
    /// Agent facts (tool calls, completed actions) are attributed to the Agent
    /// entity so they never pollute the User's cognition channel
    /// (external-knowledge-plan §C2, "agent 不替用户表态"). The `external_key`
    /// is namespaced `agent:<agent_id>` so it cannot collide with user keys
    /// (which are bare `<user_id>`).
    ///
    /// Empty ids map to `default` for backward compatibility. The returned id
    /// is stable for the same tenant/agent pair and isolated from every other
    /// pair.
    ///
    /// # Errors
    ///
    /// Returns a storage error when identity resolution fails.
    pub fn resolve_agent(&self, tenant_id: &str, agent_id: &str) -> Result<i64> {
        let tenant_id = if tenant_id.trim().is_empty() {
            "default"
        } else {
            tenant_id.trim()
        };
        let agent_id = if agent_id.trim().is_empty() {
            "default"
        } else {
            agent_id.trim()
        };
        let name = if agent_id == "default" {
            "Agent".to_string()
        } else {
            format!("Agent:{agent_id}")
        };
        // Namespace the external key so agent identity never collides with a
        // user identity that happens to share the same bare id.
        let external_key = format!("agent:{agent_id}");
        self.resolve_entity(tenant_id, Some(&external_key), &name, "agent")
    }

    /// Resolve an existing tenant-scoped entity.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the lookup fails.
    pub fn find_entity(
        &self,
        tenant_id: &str,
        external_key: Option<&str>,
        name: &str,
    ) -> Result<Option<(i64, String, String)>> {
        let conn = self.lock_conn()?;
        let mut stmt = if external_key.is_some() {
            conn.prepare(
                "SELECT id, name, entity_type FROM entities WHERE tenant_id = ?1 AND external_key = ?2",
            )?
        } else {
            conn.prepare(
                "SELECT id, name, entity_type FROM entities WHERE tenant_id = ?1 AND name = ?2 ORDER BY id LIMIT 1",
            )?
        };
        let key = external_key.unwrap_or(name);
        stmt.query_row(params![tenant_id, key], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()
        .map_err(Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify user identities are stable and tenant isolated.
    /// Invariants: Same tenant/user resolves once; changing either component changes the entity id.
    #[test]
    fn user_identity_is_stable_and_tenant_isolated() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let default_id = store
            .resolve_user("default", "")
            .expect("resolve backward-compatible root user");
        let default_again = store
            .resolve_user("default", "default")
            .expect("resolve the same root user again");
        let tenant_a_user = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve tenant A user");
        let tenant_b_user = store
            .resolve_user("tenant-b", "alice")
            .expect("resolve tenant B user");
        let tenant_a_other = store
            .resolve_user("tenant-a", "bob")
            .expect("resolve second tenant A user");

        assert_eq!(
            default_id, 1,
            "The compatible default User root must retain id 1"
        );
        assert_eq!(
            default_again, default_id,
            "Repeated identity resolution must be stable"
        );
        assert_ne!(
            tenant_a_user, tenant_b_user,
            "The same user id in different tenants must not collide"
        );
        assert_ne!(
            tenant_a_user, tenant_a_other,
            "Different users in one tenant must not collide"
        );
    }

    /// Objective: Verify agent identities are stable, tenant-isolated, and NEVER
    /// collide with a user identity that shares the same bare id — the core
    /// zero-pollution boundary for the agent fact channel.
    /// Invariants: resolve_agent is stable across calls; agent id != user id
    /// for the same bare id; tenant isolation holds; default agent resolves.
    #[test]
    fn agent_identity_is_distinct_from_user_identity() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let agent_default = store
            .resolve_agent("default", "")
            .expect("resolve default agent");
        let agent_default_again = store
            .resolve_agent("default", "default")
            .expect("resolve default agent again");
        assert_eq!(
            agent_default, agent_default_again,
            "repeated agent resolution must be stable"
        );

        // The same bare id "alice" MUST resolve to different entities when used
        // as a user vs an agent — agents never pollute the user channel.
        let user_alice = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve user alice");
        let agent_alice = store
            .resolve_agent("tenant-a", "alice")
            .expect("resolve agent alice");
        assert_ne!(
            user_alice, agent_alice,
            "agent entity must never collide with a user entity sharing the same bare id"
        );

        // Tenant isolation: the same agent id in two tenants resolves differently.
        let agent_a = store
            .resolve_agent("tenant-a", "bot")
            .expect("resolve tenant-a bot");
        let agent_b = store
            .resolve_agent("tenant-b", "bot")
            .expect("resolve tenant-b bot");
        assert_ne!(
            agent_a, agent_b,
            "the same agent id in different tenants must not collide"
        );

        // The agent entity is typed "agent", not "user".
        let conn = store.lock_conn().expect("lock");
        let entity_type: String = conn
            .query_row(
                "SELECT entity_type FROM entities WHERE id = ?1",
                params![agent_alice],
                |row| row.get(0),
            )
            .expect("read agent entity type");
        assert_eq!(
            entity_type, "agent",
            "agent entity must be typed 'agent' so it is distinguishable from users"
        );
    }
}
