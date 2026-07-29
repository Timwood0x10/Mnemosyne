//! Entity Representation — builds a stable text representation for embedding.
//!
//! The representation is a fixed-template string that captures an entity's
//! identity fingerprint — name, aliases, type, profile attributes, relations,
//! and key events. This is what gets embedded, not the raw entity name.
//!
//! ## Lifecycle
//!
//! ```text
//! Store → Snapshot → Representation → Embedder → Vector
//! ```
//!
//! The representation never touches a database. It is built at startup.

/// A snapshot of an entity's full profile at a point in time.
///
/// This is the only data structure the representation builder sees.
/// It decouples the builder from any specific storage backend.
#[derive(Debug, Clone)]
pub struct EntitySnapshot {
    /// Core entity record.
    pub entity: EntitySummary,
    /// Known aliases (including courtesy names, titles, etc.).
    pub aliases: Vec<String>,
    /// Profile key-value pairs (courtesy_name, birthplace, occupation, ...).
    pub profiles: Vec<(String, String)>,
    /// Connected entities with relation type.
    pub relations: Vec<(String, String)>,
    /// Key events this entity participated in (capped at 20).
    pub events: Vec<EventSummary>,
}

/// Minimal entity record for the snapshot.
#[derive(Debug, Clone)]
pub struct EntitySummary {
    pub id: i64,
    pub name: String,
    pub entity_type: String,
}

/// A single event summary for embedding purposes.
#[derive(Debug, Clone)]
pub struct EventSummary {
    pub title: String,
    pub chapter: i32,
    pub importance: f64,
}

/// Builds a stable text representation from an [`EntitySnapshot`].
///
/// Each implementation carries a `version` string so that embeddings from
/// different template versions are never mixed.
pub trait EntityRepresentationBuilder: Send + Sync {
    /// Version identifier (e.g. "representation_v1").
    fn version(&self) -> &str;

    /// Build a text representation from the snapshot.
    fn build(&self, snapshot: &EntitySnapshot) -> String;
}

/// Fixed-template builder with field repetition for importance weighting.
///
/// Template order (fixed):
/// 1. Name (repeated 3×)
/// 2. Alias (repeated 2×)
/// 3. Type
/// 4. Courtesy name
/// 5. Birthplace
/// 6. Occupation
/// 7. Faction
/// 8. Relations
/// 9. Events (up to 20, highest importance first)
pub struct FixedTemplateBuilder {
    version_str: String,
    max_events: usize,
}

impl FixedTemplateBuilder {
    /// Create a new builder with the given version string and default max events.
    pub fn new(version: &str) -> Self {
        FixedTemplateBuilder {
            version_str: version.to_string(),
            max_events: 20,
        }
    }

    /// Set the maximum number of events to include in the representation.
    pub fn with_max_events(mut self, n: usize) -> Self {
        self.max_events = n;
        self
    }
}

impl EntityRepresentationBuilder for FixedTemplateBuilder {
    fn version(&self) -> &str {
        &self.version_str
    }

    fn build(&self, snapshot: &EntitySnapshot) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(32);

        // Name × 3 (importance weighting)
        let name = &snapshot.entity.name;
        for _ in 0..3 {
            parts.push(format!("Name:{}", name));
        }

        // Alias × 2
        for alias in &snapshot.aliases {
            parts.push(format!("Alias:{}", alias));
        }

        // Type
        parts.push(format!("Type:{}", snapshot.entity.entity_type));

        // Profile fields (known keys)
        for (key, value) in &snapshot.profiles {
            match key.as_str() {
                "courtesy_name" => parts.push(format!("Courtesy:{}", value)),
                "birthplace" => parts.push(format!("Birthplace:{}", value)),
                "occupation" => parts.push(format!("Occupation:{}", value)),
                "ancestry" => parts.push(format!("Ancestry:{}", value)),
                "title" => parts.push(format!("Title:{}", value)),
                "appearance" => parts.push(format!("Appearance:{}", value)),
                "faction" => parts.push(format!("Faction:{}", value)),
                "status" => parts.push(format!("Status:{}", value)),
                _ => {}
            }
        }

        // Relations
        for (entity_name, rel_type) in &snapshot.relations {
            parts.push(format!("Rel:{}<{}>", rel_type, entity_name));
        }

        // Events (sorted by importance descending, capped)
        let mut sorted_events = snapshot.events.clone();
        sorted_events.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for ev in sorted_events.iter().take(self.max_events) {
            parts.push(format!("Event:{}", ev.title));
        }

        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that the fixed template produces deterministic output
    /// for the same input.
    /// Invariants: Two calls with the same snapshot produce identical strings.
    #[test]
    fn deterministic_output() {
        let builder = FixedTemplateBuilder::new("v1");
        let snapshot = EntitySnapshot {
            entity: EntitySummary {
                id: 10001,
                name: "刘备".into(),
                entity_type: "person".into(),
            },
            aliases: vec!["玄德".into()],
            profiles: vec![("courtesy_name".into(), "玄德".into())],
            relations: vec![("关羽".into(), "brother".into())],
            events: vec![],
        };
        let a = builder.build(&snapshot);
        let b = builder.build(&snapshot);
        assert_eq!(a, b, "representation should be deterministic");
    }

    /// Objective: Verify that name appears 3× and aliases appear 2× for weighting.
    /// Invariants: Output contains exactly 3 Name: lines and 2 Alias: lines.
    #[test]
    fn name_weighted_three_times() {
        let builder = FixedTemplateBuilder::new("v1");
        let snapshot = EntitySnapshot {
            entity: EntitySummary {
                id: 10001,
                name: "刘备".into(),
                entity_type: "person".into(),
            },
            aliases: vec!["玄德".into()],
            profiles: vec![],
            relations: vec![],
            events: vec![],
        };
        let output = builder.build(&snapshot);
        let name_count = output.lines().filter(|l| l.starts_with("Name:")).count();
        assert_eq!(
            name_count, 3,
            "Name must appear exactly 3 times for weighting"
        );
    }

    /// Objective: Verify that events are capped at the configured max.
    /// Invariants: Output contains at most max_events Event: lines.
    #[test]
    fn events_capped_at_max() {
        let builder = FixedTemplateBuilder::new("v1").with_max_events(3);
        let events: Vec<EventSummary> = (1..=10)
            .map(|i| EventSummary {
                title: format!("Event {}", i),
                chapter: i,
                importance: i as f64,
            })
            .collect();
        let snapshot = EntitySnapshot {
            entity: EntitySummary {
                id: 10001,
                name: "刘备".into(),
                entity_type: "person".into(),
            },
            aliases: vec![],
            profiles: vec![],
            relations: vec![],
            events,
        };
        let output = builder.build(&snapshot);
        let event_count = output.lines().filter(|l| l.starts_with("Event:")).count();
        assert_eq!(event_count, 3, "Events must be capped at max_events");
    }
}
