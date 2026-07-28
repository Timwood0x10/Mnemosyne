//! Conversation provider — discovers entities from message roles and user IDs.
//!
//! For chat/agent inputs, the user's display name or role label is treated as
//! an entity. This allows basic entity extraction without a pre-defined
//! dictionary.
//!
//! TODO: implement during Phase 3.

use super::provider::{EntityEntry, EntityProvider};

pub struct ConversationProvider;

impl EntityProvider for ConversationProvider {
    fn name(&self) -> &str {
        "conversation"
    }

    fn entries(&self) -> Vec<EntityEntry> {
        // Phase 3: extract from message roles
        Vec::new()
    }
}
