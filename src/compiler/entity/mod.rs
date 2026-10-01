//! Entity definitions for the story compiler.
//!
//! Entity definitions come from two sources, converted to a common
//! [`EntityEntry`] shape: auto-discovery over the document text
//! ([`CorpusEntityProvider`]) and the curated classical-novel character tables
//! ([`NovelProvider`]). The compiler collects their entries into an
//! [`EntityDictionary`], which maps every alias to its canonical name so
//! mentions can be resolved while scanning.

mod corpus;
mod novel;
mod provider;
mod registry;

pub use corpus::CorpusEntityProvider;
pub use novel::NovelProvider;
pub use provider::{EntityEntry, EntityProvider};
pub use registry::EntityDictionary;
