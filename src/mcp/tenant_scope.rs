//! Tenant scoping for the id-addressed read tools.
//!
//! `state_timeline`, `fact_provenance`, `decision_trace` and `decision_search`
//! address their subject by a raw entity / fact / decision id. An id carries no
//! ownership information, so without a check any client could read another
//! tenant's facts by guessing one — the same gap the decay sweep closed with
//! `all_entity_ids_in_tenant`.
//!
//! `tenant_id` stays **optional** so single-tenant deployments and existing
//! clients keep working. When it is supplied the subject must belong to that
//! tenant, and a mismatch is reported as "not found" rather than as a permission
//! error: a distinct error code would confirm that the id exists.

use crate::error::{Error, Result};
use crate::fact_store::SqliteFactStore;

/// Read the optional `tenant_id` argument.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] when the argument is present but is not a
/// non-empty string.
pub(crate) fn tenant_argument(args: &serde_json::Value) -> Result<Option<&str>> {
    match args.get("tenant_id") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => match value.as_str() {
            Some(tenant) if !tenant.trim().is_empty() => Ok(Some(tenant)),
            _ => Err(Error::InvalidInput(
                "`tenant_id` must be a non-empty string".into(),
            )),
        },
    }
}

/// Refuse a subject that does not belong to the requested tenant.
///
/// Does nothing when no `tenant_id` was requested.
///
/// # Errors
///
/// Returns [`Error::NotFound`] when the entity belongs to another tenant or does
/// not exist.
pub(crate) fn ensure_entity_tenant(
    store: &SqliteFactStore,
    entity_id: i64,
    tenant_id: Option<&str>,
) -> Result<()> {
    let Some(tenant) = tenant_id else {
        return Ok(());
    };
    match store.entity_tenant(entity_id)? {
        Some(owner) if owner == tenant => Ok(()),
        _ => Err(Error::NotFound(format!(
            "no entity {entity_id} in tenant `{tenant}`"
        ))),
    }
}
