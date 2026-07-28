//! Entity Engine — Phase 3.
//!
//! Discovers entity mentions in text using an [`EntityRegistry`] that merges
//! multiple [`EntityProvider`]s.

mod registry;
mod provider;
mod novel;
mod conversation;
mod regex;

pub use registry::EntityRegistry;
pub use provider::EntityProvider;
pub use novel::NovelProvider;
pub use conversation::ConversationProvider;
pub use regex::RegexProvider;
