//! Entity provider trait. Each provider returns a list of known entities
//! and their aliases, which the compiler merges into an
//! [`EntityDictionary`](super::EntityDictionary).

use std::collections::HashMap;

/// One entry in an entity dictionary.
#[derive(Debug, Clone)]
pub struct EntityEntry {
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub single_char: Option<String>,
    pub object_type: String,
    pub properties: HashMap<String, String>,
}

/// A provider of entity definitions.
///
/// Implementations should be stateless or cheap to construct: the compiler
/// calls [`entries`](EntityProvider::entries) once per compile and merges the
/// results into an [`EntityDictionary`](super::EntityDictionary).
pub trait EntityProvider: Send + Sync {
    fn name(&self) -> &str;
    fn entries(&self) -> Vec<EntityEntry>;
}
