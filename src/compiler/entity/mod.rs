//! Entity Engine — Phase 3.
//!
//! Discovers entity mentions in text using an [`EntityRegistry`] that merges
//! multiple [`EntityProvider`]s.

mod conversation;
mod novel;
mod provider;
mod regex;
mod registry;

pub use conversation::ConversationProvider;
pub use novel::NovelProvider;
pub use provider::EntityProvider;
pub use regex::RegexProvider;
pub use registry::EntityRegistry;
