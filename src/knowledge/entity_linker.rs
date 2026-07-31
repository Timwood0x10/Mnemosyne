//! Entity linker — resolves external surface names to unified graph entities.
//!
//! External sources (documents, DBs, AI conversations) refer to entities by
//! their own surface names ("John Smith", "J. Smith", "Mr. Smith"). The
//! [`EntityLinker`] aggregates [`EntityLink`]s contributed by every registered
//! [`KnowledgeAdapter`] and resolves any surface to a single canonical name,
//! so `inspect_entity` can treat cross-source mentions as one graph node
//! (external-knowledge-plan §D: "外部实体名 → 统一图谱节点").
//!
//! ## Resolution policy
//!
//! - **Source-scoped lookup wins**: `(source, surface)` is checked first so
//!   identical surface names that different sources map differently are
//!   disambiguated (e.g. CRM "John Smith" → "Mr. Smith" vs HR "John Smith"
//!   → "Johnny").
//! - **Surface-only fallback**: when no source is supplied, the first
//!   registration for a surface wins. This exposes a stable canonical even
//!   when multiple sources disagree, mirroring a "first-link-wins" policy.
//! - **Canonical passthrough**: a surface that already IS a canonical name
//!   resolves to itself, so `inspect_entity` callers that pass the canonical
//!   name directly are unaffected.
//!
//! ## Provenance
//!
//! The reverse index (`canonical -> Vec<EntityLink>`) lets `inspect_entity`
//! report every (source, surface) pair that feeds a unified entity, satisfying
//! the plan's "返回跨来源画像" requirement.

use std::collections::HashMap;

use crate::knowledge::adapter::EntityLink;

// ───────────────────────────────────────────────────────────────────────────
// EntityLinker
// ───────────────────────────────────────────────────────────────────────────

/// Cross-source entity resolver.
///
/// Built from the [`EntityLink`]s collected by
/// [`crate::knowledge::ExternalKnowledgeRegistry::collect_entity_links`]. The
/// linker is a pure value type: clone it, share it via `Arc`, or rebuild it
/// whenever the registry changes. Runtime mutation happens at the MCP layer
/// (e.g. `knowledge_attach` rebuilds the linker after attaching a source).
pub struct EntityLinker {
    /// `(source, external_name) -> canonical_name`. Source-scoped lookups
    /// disambiguate identical surface names that different sources map
    /// differently. Last registration wins per (source, surface) pair so a
    /// re-attached source can correct a stale mapping.
    source_map: HashMap<(String, String), String>,
    /// `external_name -> canonical_name`. Fallback for surface-only lookups
    /// (no source). First registration wins so the canonical is stable across
    /// re-registrations of the same surface from different sources.
    surface_map: HashMap<String, String>,
    /// `canonical_name -> Vec<EntityLink>`. Reverse index for provenance
    /// reporting (which sources and surfaces feed this canonical entity).
    /// Deduped by (source, external_name) so repeated registrations do not
    /// inflate the provenance list.
    reverse: HashMap<String, Vec<EntityLink>>,
}

impl EntityLinker {
    /// Create an empty linker.
    #[must_use]
    pub fn new() -> Self {
        Self {
            source_map: HashMap::new(),
            surface_map: HashMap::new(),
            reverse: HashMap::new(),
        }
    }

    /// Build a linker from a batch of [`EntityLink`]s (typically the output of
    /// `ExternalKnowledgeRegistry::collect_entity_links`).
    ///
    /// Links are registered in iteration order; the resulting source/surface
    /// maps follow the policies documented at the module level.
    #[must_use]
    pub fn from_links(links: Vec<EntityLink>) -> Self {
        let mut linker = Self::new();
        for link in links {
            linker.register(link);
        }
        linker
    }

    /// Register one [`EntityLink`].
    ///
    /// - `source_map[(source, external_name)]` is overwritten (last wins) so a
    ///   re-attached source can correct a stale mapping.
    /// - `surface_map[external_name]` is set only on the FIRST registration
    ///   for that surface (first wins) so the canonical stays stable.
    /// - `reverse[canonical_name]` accumulates deduped links for provenance.
    pub fn register(&mut self, link: EntityLink) {
        let key = (link.source.clone(), link.external_name.clone());
        self.source_map.insert(key, link.canonical_name.clone());
        // First-link-wins for the surface-only fallback: only insert if the
        // surface is not yet known. This keeps a stable canonical even when a
        // later source registers the same surface with a different canonical.
        self.surface_map
            .entry(link.external_name.clone())
            .or_insert_with(|| link.canonical_name.clone());
        // Reverse index: dedup by (source, external_name) so repeated
        // registrations of the same link do not inflate provenance.
        let bucket = self.reverse.entry(link.canonical_name.clone()).or_default();
        let already = bucket.iter().any(|existing| {
            existing.source == link.source && existing.external_name == link.external_name
        });
        if !already {
            bucket.push(link);
        }
    }

    /// Resolve a surface name (optionally source-scoped) to its canonical name.
    ///
    /// Resolution order:
    /// 1. `(source, surface)` exact match (source-scoped).
    /// 2. `surface` fallback (first-link-wins).
    /// 3. `None` if the surface is unknown to any source.
    ///
    /// Returns the canonical name as `&str`, or `None` when no link matches.
    #[must_use]
    pub fn resolve<'a>(&'a self, surface: &str, source: Option<&str>) -> Option<&'a str> {
        if let Some(src) = source {
            if let Some(canonical) = self.source_map.get(&(src.to_string(), surface.to_string())) {
                return Some(canonical.as_str());
            }
        }
        self.surface_map.get(surface).map(String::as_str)
    }

    /// Returns `true` when the linker holds no links.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.source_map.is_empty()
    }

    /// Number of distinct canonical entities known to the linker.
    #[must_use]
    pub fn len(&self) -> usize {
        self.reverse.len()
    }

    /// All canonical names known to the linker, in arbitrary (HashMap) order.
    #[must_use]
    pub fn canonical_names(&self) -> Vec<&str> {
        self.reverse.keys().map(String::as_str).collect()
    }

    /// Provenance for a canonical name: every `(source, external_name)` pair
    /// that maps to it. Returns an empty slice when the canonical is unknown.
    #[must_use]
    pub fn provenance(&self, canonical: &str) -> &[EntityLink] {
        self.reverse
            .get(canonical)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Returns the surface names (without source) that resolve to `canonical`.
    /// Useful for `inspect_entity` to list the external aliases of a node.
    #[must_use]
    pub fn surfaces_for(&self, canonical: &str) -> Vec<&str> {
        self.reverse
            .get(canonical)
            .map(|links| links.iter().map(|l| l.external_name.as_str()).collect())
            .unwrap_or_default()
    }
}

impl Default for EntityLinker {
    fn default() -> Self {
        Self::new()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn link(surface: &str, canonical: &str, source: &str) -> EntityLink {
        EntityLink {
            external_name: surface.into(),
            canonical_name: canonical.into(),
            source: source.into(),
        }
    }

    /// Objective: Verify a freshly-constructed linker reports empty state and
    /// resolves nothing.
    /// Invariants: is_empty() true; len() 0; resolve returns None for any
    /// surface/source combination.
    #[test]
    fn new_linker_is_empty() {
        let linker = EntityLinker::new();
        assert!(linker.is_empty(), "fresh linker is empty");
        assert_eq!(linker.len(), 0, "no canonical entities");
        assert!(
            linker.resolve("John", None).is_none(),
            "empty linker resolves nothing"
        );
        assert!(
            linker.resolve("John", Some("crm")).is_none(),
            "empty linker resolves nothing even with a source"
        );
        assert!(linker.canonical_names().is_empty(), "no canonical names");
        assert!(
            linker.provenance("anyone").is_empty(),
            "provenance is empty for unknown canonical"
        );
    }

    /// Objective: Verify the canonical "John Smith" <-> "Mr. Smith" link from
    /// the plan (§D4): an external surface resolves to the unified graph name.
    /// Invariants: resolve(surface, source) returns the canonical; resolve
    /// without a source also returns the canonical via the surface fallback.
    #[test]
    fn resolves_external_surface_to_canonical() {
        let linker = EntityLinker::from_links(vec![
            link("John Smith", "Mr. Smith", "crm"),
            link("J. Smith", "Mr. Smith", "novel"),
        ]);

        // Source-scoped resolution.
        assert_eq!(
            linker.resolve("John Smith", Some("crm")),
            Some("Mr. Smith"),
            "CRM surface resolves to the canonical"
        );
        assert_eq!(
            linker.resolve("J. Smith", Some("novel")),
            Some("Mr. Smith"),
            "novel surface resolves to the same canonical"
        );
        // Surface-only fallback.
        assert_eq!(
            linker.resolve("John Smith", None),
            Some("Mr. Smith"),
            "surface-only lookup falls back to the canonical"
        );
        assert_eq!(
            linker.resolve("J. Smith", None),
            Some("Mr. Smith"),
            "second surface also falls back to the canonical"
        );
        assert_eq!(linker.len(), 1, "one canonical entity");
    }

    /// Objective: Verify source-scoped lookup disambiguates identical surface
    /// names that different sources map to DIFFERENT canonicals.
    /// Invariants: "John Smith" from CRM -> "Mr. Smith"; "John Smith" from HR
    /// -> "Johnny"; surface-only fallback returns the FIRST-registered
    /// canonical ("Mr. Smith").
    #[test]
    fn source_scoped_lookup_disambiguates_same_surface() {
        let linker = EntityLinker::from_links(vec![
            link("John Smith", "Mr. Smith", "crm"),
            link("John Smith", "Johnny", "hr"),
        ]);

        assert_eq!(
            linker.resolve("John Smith", Some("crm")),
            Some("Mr. Smith"),
            "CRM-scoped lookup returns the CRM canonical"
        );
        assert_eq!(
            linker.resolve("John Smith", Some("hr")),
            Some("Johnny"),
            "HR-scoped lookup returns the HR canonical"
        );
        // First-link-wins for the surface-only fallback.
        assert_eq!(
            linker.resolve("John Smith", None),
            Some("Mr. Smith"),
            "surface-only fallback returns the first-registered canonical"
        );
        assert_eq!(linker.len(), 2, "two distinct canonical entities");
    }

    /// Objective: Verify resolve returns None for an unknown surface even when
    /// a source is supplied, and that a known surface with an unknown source
    /// falls back to the surface-only map.
    /// Invariants: unknown surface -> None; known surface + unknown source ->
    /// surface fallback.
    #[test]
    fn unknown_surface_returns_none() {
        let linker = EntityLinker::from_links(vec![link("John Smith", "Mr. Smith", "crm")]);

        assert!(
            linker.resolve("Jane Doe", Some("crm")).is_none(),
            "unknown surface resolves to None even with a known source"
        );
        assert!(
            linker.resolve("Jane Doe", None).is_none(),
            "unknown surface with no source resolves to None"
        );
        // Known surface but unknown source falls back to the surface map.
        assert_eq!(
            linker.resolve("John Smith", Some("unknown-source")),
            Some("Mr. Smith"),
            "known surface with unknown source falls back to surface map"
        );
    }

    /// Objective: Verify provenance reports every (source, surface) pair that
    /// maps to a canonical, and that repeated registrations do not duplicate.
    /// Invariants: provenance for "Mr. Smith" lists CRM+novel; re-registering
    /// the CRM link does not grow the list.
    #[test]
    fn provenance_lists_all_surfaces_deduped() {
        let mut linker = EntityLinker::new();
        linker.register(link("John Smith", "Mr. Smith", "crm"));
        linker.register(link("J. Smith", "Mr. Smith", "novel"));
        // Re-register the same CRM link — must NOT duplicate.
        linker.register(link("John Smith", "Mr. Smith", "crm"));

        let prov = linker.provenance("Mr. Smith");
        assert_eq!(
            prov.len(),
            2,
            "provenance dedups identical (source, surface) pairs"
        );
        // surfaces_for returns the external names without source.
        let mut surfaces: Vec<&str> = linker.surfaces_for("Mr. Smith").into_iter().collect();
        surfaces.sort_unstable();
        assert_eq!(surfaces, vec!["J. Smith", "John Smith"]);

        // Unknown canonical returns empty provenance.
        assert!(
            linker.provenance("nobody").is_empty(),
            "unknown canonical has empty provenance"
        );
        assert!(
            linker.surfaces_for("nobody").is_empty(),
            "unknown canonical has no surfaces"
        );
    }

    /// Objective: Verify source_map last-wins lets a re-attached source
    /// correct a stale mapping, while the surface-only fallback stays stable.
    /// Invariants: re-registering (crm, "John Smith") with a new canonical
    /// overwrites the source-scoped entry but leaves the surface fallback
    /// pointing at the original first-registered canonical.
    #[test]
    fn re_registration_overwrites_source_map_but_keeps_surface_first_wins() {
        let mut linker = EntityLinker::new();
        linker.register(link("John Smith", "Mr. Smith", "crm"));
        // CRM re-attaches with a corrected canonical.
        linker.register(link("John Smith", "John Smith Sr.", "crm"));

        assert_eq!(
            linker.resolve("John Smith", Some("crm")),
            Some("John Smith Sr."),
            "source-scoped lookup reflects the corrected canonical (last wins)"
        );
        // Surface-only fallback stays at the first-registered canonical.
        assert_eq!(
            linker.resolve("John Smith", None),
            Some("Mr. Smith"),
            "surface-only fallback keeps the first-registered canonical (first wins)"
        );
        assert_eq!(linker.len(), 2, "two canonicals after the correction");
    }

    /// Objective: Verify Default trait yields an empty linker equivalent to new().
    /// Invariants: default().is_empty() == true.
    #[test]
    fn default_is_empty() {
        let linker = EntityLinker::default();
        assert!(linker.is_empty(), "default linker is empty");
    }

    /// Objective: Verify canonical_names returns every distinct canonical.
    /// Invariants: two links to the same canonical + one to a different
    /// canonical yields two distinct names.
    #[test]
    fn canonical_names_lists_distinct_canonicals() {
        let linker = EntityLinker::from_links(vec![
            link("John Smith", "Mr. Smith", "crm"),
            link("J. Smith", "Mr. Smith", "novel"),
            link("Anna K", "Anna Karenina", "novel"),
        ]);
        let mut names: Vec<&str> = linker.canonical_names().into_iter().collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["Anna Karenina", "Mr. Smith"],
            "distinct canonicals are listed"
        );
    }
}
