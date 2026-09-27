//! Tenant scoping for the id-addressed tools.
//!
//! This engine runs as a **single-node MCP server**: one installation serves one
//! tenant, and `tenant_id` is a data label rather than an authorization
//! boundary. What the tools still need is *consistency* — a subject addressed by
//! a raw id (entity / fact / decision) must carry the label the caller is
//! working in, so a client naming one tenant is never handed, and never
//! rewrites, a row labelled with another.
//!
//! The argument used to be **optional**, and an omitted `tenant_id` skipped the
//! check outright: the guard looked like isolation while any client could step
//! around it by simply leaving the field out (audit C1). It is resolved through
//! [`crate::mcp::types::identity_arg`] now — the same helper every other tool
//! uses for `tenant_id` / `user_id` — so the check always runs and an omitted
//! value means the local tenant rather than "unscoped".

use crate::error::{Error, Result};
use crate::fact_store::SqliteFactStore;

/// Refuse a subject that is not labelled with the caller's tenant.
///
/// The caller's tenant comes from the request and defaults to the local one, so
/// there is no "unscoped" spelling to fall back to.
///
/// # Errors
///
/// Returns [`Error::NotFound`] when the entity carries another label or does not
/// exist. A mismatch is deliberately reported as *not found* rather than as a
/// permission error: a distinct code would confirm that the id exists.
pub(crate) fn ensure_entity_tenant(
    store: &SqliteFactStore,
    entity_id: i64,
    tenant_id: &str,
) -> Result<()> {
    match store.entity_tenant(entity_id)? {
        Some(owner) if owner == tenant_id => Ok(()),
        _ => Err(Error::NotFound(format!(
            "no entity {entity_id} in tenant `{tenant_id}`"
        ))),
    }
}
