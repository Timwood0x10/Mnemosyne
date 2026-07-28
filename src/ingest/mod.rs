//! Character knowledge graph distillation from classical Chinese novel corpus.
//!
//! Reads corpus text files, extracts character appearances, events,
//! descriptions, and relationships, then persists into the character
//! store tables (same schema used by `character_search` / `character_network`).

pub mod characters;
pub mod corpus;
pub mod extract;
pub mod relation;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use crate::character::{
    CharacterAttribute, CharacterEvent, CharacterRelation, CharacterStore, RelationSource,
    SQLiteCharacterStore,
};
use crate::error::Result;
use crate::faction;
use crate::types::Metadata;

/// Overall ingestion statistics.
#[derive(Debug, Default, Clone, Serialize)]
pub struct IngestionStats {
    pub characters: usize,
    pub events: usize,
    pub relations: usize,
}

impl std::ops::AddAssign for IngestionStats {
    fn add_assign(&mut self, other: Self) {
        self.characters += other.characters;
        self.events += other.events;
        self.relations += other.relations;
    }
}

/// Internal event entry tracked per character.
#[derive(Debug, Clone)]
struct EventEntry {
    chapter: i32,
    event_name: String,
    description: String,
    related: Vec<String>,
}

/// Per-character tracking state during novel distillation.
#[derive(Debug, Clone)]
struct CharInfo {
    aliases: Vec<String>,
    chapters: HashSet<i32>,
    events: Vec<EventEntry>,
    clothing: HashSet<String>,
    personality: HashSet<String>,
    co_occur: HashMap<String, usize>,
    /// Best context (containing both names) per pair, key sorted alphabetically.
    /// Updated only when a more specific relation type is detected, so the
    /// first chapter's context is replaced if a later chapter reveals the
    /// actual relationship (e.g. "结义" only becomes visible in chapter 1 of
    /// 三国演义 even though 刘备 and 关羽 may have co-occurred earlier in
    /// introductory text).
    relation_text: HashMap<(String, String), String>,
    /// Best detected relation type per pair, key sorted alphabetically.
    /// Empty/"关联" until a chapter yields a keyword near both names.
    relation_type: HashMap<(String, String), String>,
    /// Chapter where the best relation type was detected.
    relation_chapter: HashMap<(String, String), i32>,
    death_chapter: Option<i32>,
    death_desc: Option<String>,
    _last_death_check: Option<i32>,
}

impl CharInfo {
    fn new(aliases: Vec<String>) -> Self {
        Self {
            aliases,
            chapters: HashSet::new(),
            events: Vec::new(),
            clothing: HashSet::new(),
            personality: HashSet::new(),
            co_occur: HashMap::new(),
            relation_text: HashMap::new(),
            relation_type: HashMap::new(),
            relation_chapter: HashMap::new(),
            death_chapter: None,
            death_desc: None,
            _last_death_check: None,
        }
    }

    /// Return the currently stored relation type for `key`, defaulting to
    /// "关联" (generic) when no entry exists yet.
    fn relation_type_for(&self, key: &(String, String)) -> &str {
        self.relation_type
            .get(key)
            .map(|s| s.as_str())
            .unwrap_or("关联")
    }
}

/// Alias match result with position and resolution.
#[derive(Debug, Clone)]
struct AliasMatch {
    start: usize,
    end: usize,
    alias: String,
    character: String,
}

/// Pipeline that distills character knowledge from corpus text files.
pub struct IngestionPipeline {
    store: Arc<SQLiteCharacterStore>,
    corpus_dir: PathBuf,
}

impl IngestionPipeline {
    /// Create a new ingestion pipeline.
    pub fn new(store: Arc<SQLiteCharacterStore>, corpus_dir: &str) -> Self {
        Self {
            store,
            corpus_dir: PathBuf::from(corpus_dir),
        }
    }

    /// Run full distillation over all four novels.
    pub async fn run(&self) -> Result<IngestionStats> {
        let mut totals = IngestionStats::default();
        for novel in characters::NOVELS {
            let r = self.distill_novel(novel).await?;
            tracing::info!(
                "{novel}: {} chars, {} events, {} relations",
                r.characters,
                r.events,
                r.relations
            );
            totals += r;
        }
        Ok(totals)
    }

    /// Distill a single novel.
    ///
    /// Unknown novel names return empty stats (not an error), so the pipeline
    /// can iterate over a fixed list without crashing on unsupported titles.
    async fn distill_novel(&self, novel_name: &str) -> Result<IngestionStats> {
        // Unknown novels → empty stats (not an error)
        if corpus::novel_filename(novel_name).is_none() {
            return Ok(IngestionStats::default());
        }
        let chapters =
            corpus::load_novel(novel_name, &self.corpus_dir).map_err(crate::error::Error::Io)?;
        if chapters.is_empty() {
            return Ok(IngestionStats::default());
        }

        let cdefs = characters::get_novel_characters(novel_name);
        let tenant_id = "novels";
        let now_ts = Utc::now();

        // Build longest-first search names per character
        let char_search_names: HashMap<&str, Vec<String>> = cdefs
            .iter()
            .map(|cdef| {
                let mut all: Vec<String> = std::iter::once(cdef.name.to_string())
                    .chain(cdef.aliases.iter().map(|a| a.to_string()))
                    .filter(|s| !s.is_empty())
                    .collect();
                all.sort_by_key(|b| std::cmp::Reverse(b.len()));
                (cdef.name, all)
            })
            .collect();

        // Build (alias→canonical) pairs for dialog-chain relation extraction
        let mut name_pairs: Vec<(String, String)> = char_search_names
            .iter()
            .flat_map(|(canonical, aliases)| {
                aliases
                    .iter()
                    .map(move |a| (a.clone(), canonical.to_string()))
            })
            .collect();
        name_pairs.sort_by_key(|(alias, _)| std::cmp::Reverse(alias.len()));

        // Initialize char info
        let mut char_info: HashMap<&str, CharInfo> = HashMap::new();
        for cdef in cdefs {
            let aliases: Vec<String> = std::iter::once(cdef.name.to_string())
                .chain(cdef.aliases.iter().map(|a| a.to_string()))
                .filter(|s| !s.is_empty())
                .collect();
            char_info.insert(cdef.name, CharInfo::new(aliases));
        }

        // Scan chapters
        let t0 = std::time::Instant::now();
        for ch in &chapters {
            self.process_chapter(
                ch,
                novel_name,
                &char_search_names,
                &mut char_info,
                &name_pairs,
            )?;
        }
        let scan_ms = t0.elapsed().as_millis();

        // Insert into DB
        let t1 = std::time::Instant::now();
        let mut stats = IngestionStats::default();
        stats += self
            .insert_characters(novel_name, tenant_id, now_ts, &char_info)
            .await?;
        let insert_ms = t1.elapsed().as_millis();
        eprintln!("[{novel_name}] scan={scan_ms}ms insert={insert_ms}ms");
        Ok(stats)
    }

    /// Process a single chapter: resolve aliases, extract events, track co-occurrence,
    /// and extract directed relations from dialog chain.
    #[allow(clippy::too_many_arguments)]
    fn process_chapter(
        &self,
        ch: &corpus::Chapter,
        novel_name: &str,
        char_search_names: &HashMap<&str, Vec<String>>,
        char_info: &mut HashMap<&str, CharInfo>,
        name_pairs: &[(String, String)],
    ) -> Result<()> {
        let text = &ch.text;
        let _t0 = std::time::Instant::now();
        let _profile = ch.num <= 3;

        // Find all alias matches with positions — single pass via Aho-Corasick
        // instead of scanning the full text once per alias (~150 scans per chapter).
        // Preserves exact behavior: overlapping matches are resolved below.
        let mut ac_patterns: Vec<&str> = Vec::new();
        let mut ac_map: Vec<(String, String)> = Vec::new();
        for (&name, search_names) in char_search_names {
            for sn in search_names {
                ac_patterns.push(sn.as_str());
                ac_map.push((name.to_string(), sn.clone()));
            }
        }
        let ac = aho_corasick::AhoCorasick::builder()
            .match_kind(aho_corasick::MatchKind::Standard)
            .build(&ac_patterns)
            .expect("Aho-Corasick automaton build should never fail");
        let mut alias_matches: Vec<AliasMatch> = Vec::new();
        for m in ac.find_overlapping_iter(text) {
            let (name, alias) = &ac_map[m.pattern().as_usize()];
            alias_matches.push(AliasMatch {
                start: m.start(),
                end: m.end(),
                alias: alias.clone(),
                character: name.clone(),
            });
        }

        // Add safe single-char shortname matches (e.g. "飞曰"→张飞, "瑜怒"→周瑜).
        //
        // Single chars are recognized only when (1) preceded by punctuation /
        // whitespace / string-start and (2) followed by a dialog or action
        // verb. This avoids false positives on multi-char names that contain
        // the same character (e.g. "云长" for 关羽, vs. "云" for 赵云). See
        // [`extract::find_single_char_matches`] for the full safety contract.
        let cdefs = characters::get_novel_characters(novel_name);
        let single_char_matches = extract::find_single_char_matches(text, cdefs);
        for (start, end, name) in single_char_matches {
            alias_matches.push(AliasMatch {
                start,
                end,
                alias: name.clone(),
                character: name,
            });
        }

        // Resolve overlaps: longest match wins
        alias_matches.sort_by_key(|m| (m.start, std::cmp::Reverse(m.alias.len())));
        let mut resolved: Vec<AliasMatch> = Vec::new();
        let mut last_end = 0usize;
        for m in alias_matches {
            if m.start >= last_end {
                resolved.push(m.clone());
                last_end = m.end;
            }
        }

        // Group resolved matches by character name
        let mut chars_positions: HashMap<String, Vec<(usize, usize, String)>> = HashMap::new();
        let mut chars_in_chapter: HashSet<String> = HashSet::new();
        for m in &resolved {
            chars_in_chapter.insert(m.character.clone());
            if let Some(info) = char_info.get_mut(m.character.as_str()) {
                info.chapters.insert(ch.num);
            }
            chars_positions
                .entry(m.character.clone())
                .or_default()
                .push((m.start, m.end, m.alias.clone()));
        }
        if _profile { eprintln!("  ch{} t=aliases: {}ms", ch.num, _t0.elapsed().as_millis()); }

        // Process events per character (max 1 per chapter)
        for (name, positions) in &chars_positions {
            let info = char_info.get_mut(name.as_str()).ok_or_else(|| {
                crate::error::Error::Internal(format!("missing char info for {name}"))
            })?;

            if let Some((_st, _en, alias)) = positions.first() {
                // Event extraction
                if let Some(ev_sent) = extract::extract_action_sentence(text, alias) {
                    let related: Vec<String> = chars_in_chapter
                        .iter()
                        .filter(|o| *o != name)
                        .cloned()
                        .collect();
                    let ev_name = format!(
                        "第{}回 {}",
                        ch.num,
                        ev_sent.chars().take(50).collect::<String>()
                    );

                    // Deduplicate: only one event per chapter per character
                    let exists = info.events.iter().any(|e| e.chapter == ch.num);
                    if !exists {
                        info.events.push(EventEntry {
                            chapter: ch.num,
                            event_name: ev_name,
                            description: ev_sent.chars().take(300).collect(),
                            related,
                        });
                    }
                }

                // Description extraction
                let (clothing, personality) = extract::extract_description(text, alias);
                if !clothing.is_empty() {
                    info.clothing.insert(clothing);
                }
                if !personality.is_empty() {
                    info.personality.insert(personality);
                }
            }

            // Death detection
            for (_st, en, _al) in positions {
                for dkw in extract::DEATH_KW {
                    // Floor to char boundary to avoid panicking on multi-byte text
                    let context_end =
                        extract::floor_char_boundary(text, std::cmp::min(text.len(), *en + 100));
                    if text[*en..context_end].contains(dkw) {
                        if info._last_death_check.is_none_or(|last| ch.num > last) {
                            info.death_chapter = Some(ch.num);
                            info.death_desc = extract::extract_action_sentence(text, name)
                                .map(|s| s.chars().take(200).collect());
                            info._last_death_check = Some(ch.num);
                        }
                        break;
                    }
                }
            }
        }
        if _profile { eprintln!("  ch{} t=events: {}ms", ch.num, _t0.elapsed().as_millis()); }

        // Build relation index once per chapter (not per pair) for O(N²)-free
        // relation type detection. Pre-computes keyword and character positions
        // from the alias-matching phase.
        let relation_index = relation::ChapterRelationIndex::build(text);
        let char_positions_simple: HashMap<String, Vec<usize>> = chars_positions
            .iter()
            .map(|(name, positions)| {
                let starts: Vec<usize> = positions.iter().map(|(s, _, _)| *s).collect();
                (name.clone(), starts)
            })
            .collect();

        // Co-occurrence tracking for relations
        let names: Vec<&str> = chars_in_chapter.iter().map(|s| s.as_str()).collect();
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                let a = names[i];
                let b = names[j];
                let key = if a < b {
                    (a.to_string(), b.to_string())
                } else {
                    (b.to_string(), a.to_string())
                };

                // Increment co-occurrence count
                if let Some(info) = char_info.get_mut(a) {
                    *info.co_occur.entry(b.to_string()).or_insert(0) += 1;
                }
                if let Some(info) = char_info.get_mut(b) {
                    *info.co_occur.entry(a.to_string()).or_insert(0) += 1;
                }

                // Track best relation type for this pair across all chapters.
                //
                // The first chapter where two characters co-occur may not
                // contain a relation keyword (e.g. an early meeting scene). We
                // therefore re-evaluate every chapter until a specific type
                // (non-"关联") is detected, then lock it in. This is what
                // lets 刘备/关羽 resolve to "结义" from chapter 1's oath text
                // even though they share many generic co-occurrences later.
                //
                // Type detection runs on the FULL chapter text (not just the
                // narrow `find_relation_context` window) so a keyword can be
                // matched anywhere in the chapter — the narrow window is only
                // used for the human-readable `relation_text` field.
                let current_is_generic = char_info
                    .get(a)
                    .map(|info| info.relation_type_for(&key) == "关联")
                    .unwrap_or(true);
                let has_context = char_info
                    .get(a)
                    .map(|info| info.relation_text.contains_key(&key))
                    .unwrap_or(false);
                if current_is_generic {
                    let new_type = relation_index.detect_type(&char_positions_simple, a, b);
                    let ctx = relation::find_relation_context_indexed(
                        text,
                        &char_positions_simple,
                        a,
                        b,
                    );
                    // Store when: (1) no context yet, or (2) we found a
                    // specific type that should replace the generic one.
                    let should_store = !has_context || new_type != "关联";
                    if should_store {
                        let ctx_str = ctx.unwrap_or_default();
                        if let Some(info_a) = char_info.get_mut(a) {
                            info_a.relation_text.insert(key.clone(), ctx_str.clone());
                            info_a.relation_type.insert(key.clone(), new_type.clone());
                            info_a.relation_chapter.insert(key.clone(), ch.num);
                        }
                        if let Some(info_b) = char_info.get_mut(b) {
                            info_b.relation_text.insert(key.clone(), ctx_str);
                            info_b.relation_type.insert(key.clone(), new_type.clone());
                            info_b.relation_chapter.insert(key.clone(), ch.num);
                        }
                    }
                }
            }
        }
        if _profile { eprintln!("  ch{} t=pairs: {}ms", ch.num, _t0.elapsed().as_millis()); }

        // Dialog-chain-based directed relation extraction.
        //
        // Parses every `曰：` occurrence, builds a dialog chain, identifies address
        // keywords (主公/哥哥/师父/陛下 etc.), and resolves the addressee from
        // dialog context (`对曰` = reply to previous speaker, `谓X曰` = explicit).
        // Overrides relation types from generic/ambiguous proximity to precise
        // directed relations.
        let (dialog_relations, _dialog_cost) = if _profile {
            let t0 = std::time::Instant::now();
            let r = relation::extract_dialog_relations(text, name_pairs);
            let ms = t0.elapsed().as_micros() as f64 / 1000.0;
            (r, ms)
        } else {
            (relation::extract_dialog_relations(text, name_pairs), 0.0)
        };
        if _profile {
            let n_dm = text.matches("曰：").count();
            eprintln!("  ch{} t=dialog_extract: {:.0}ms {}rels ({}曰: {}name_pairs)", ch.num, _dialog_cost, dialog_relations.len(), n_dm, name_pairs.len());
        }
        for dr in &dialog_relations {
            let key = if dr.speaker < dr.addressee {
                (dr.speaker.clone(), dr.addressee.clone())
            } else {
                (dr.addressee.clone(), dr.speaker.clone())
            };

            // Only override if currently generic
            let current_is_generic = char_info
                .get(dr.speaker.as_str())
                .map(|info| info.relation_type_for(&key) == "关联")
                .unwrap_or(true);
            let has_context = char_info
                .get(dr.speaker.as_str())
                .map(|info| info.relation_text.contains_key(&key))
                .unwrap_or(false);

            if current_is_generic || !has_context {
                let ctx = relation::find_relation_context_indexed(
                    text,
                    &char_positions_simple,
                    &dr.speaker,
                    &dr.addressee,
                );
                let ctx_str = ctx.unwrap_or_default();
                for name in [dr.speaker.as_str(), dr.addressee.as_str()] {
                    if let Some(info) = char_info.get_mut(name) {
                        info.relation_text.insert(key.clone(), ctx_str.clone());
                        info.relation_type
                            .insert(key.clone(), dr.relation_type.clone());
                        info.relation_chapter.insert(key.clone(), ch.num);
                    }
                }
            }
        }

        if _profile { eprintln!("  ch{} t=final: {}ms", ch.num, _t0.elapsed().as_millis()); }
        Ok(())
    }

    /// Insert all tracked character data into the database.
    async fn insert_characters(
        &self,
        novel_name: &str,
        tenant_id: &str,
        now_ts: chrono::DateTime<Utc>,
        char_info: &HashMap<&str, CharInfo>,
    ) -> Result<IngestionStats> {
        let mut stats = IngestionStats::default();

        // Track which (a, b) pairs have already been inserted to avoid
        // duplicating bidirectional relations: when iterating `char_info`,
        // both 诸葛亮→刘备 and 刘备→诸葛亮 would otherwise produce two rows.
        // Relations are stored with `bidirections=true`, so one row per pair
        // is sufficient. The key is the alphabetically-sorted name tuple.
        let mut inserted_pairs: HashSet<(String, String)> = HashSet::new();

        for (&name, info) in char_info {
            let n_chapters = info.chapters.len();
            if n_chapters == 0 {
                continue;
            }

            let mut clothing_parts: Vec<&str> = info.clothing.iter().map(|s| s.as_str()).collect();
            clothing_parts.sort();
            let clothing: String = clothing_parts.join("; ").chars().take(500).collect();
            let mut personality_parts: Vec<&str> =
                info.personality.iter().map(|s| s.as_str()).collect();
            personality_parts.sort();
            let personality: String = personality_parts.join("; ").chars().take(500).collect();

            let importance = (n_chapters as f64 / 30.0).min(1.0);

            let mut desc_parts = vec![format!("登场{novel_name}共{n_chapters}回")];
            if let Some(dc) = info.death_chapter {
                desc_parts.push(format!("卒于第{dc}回"));
            }
            let description = desc_parts.join("；");

            let char_attr = CharacterAttribute {
                id: Uuid::new_v4().to_string(),
                tenant_id: tenant_id.to_string(),
                name: name.to_string(),
                novel: novel_name.to_string(),
                aliases: info.aliases.clone(),
                clothing,
                personality,
                description,
                importance,
                created_at: now_ts,
                metadata: Metadata::default(),
            };
            self.store.create_character(&char_attr).await.ok();
            stats.characters += 1;

            // Insert events
            for ev in &info.events {
                let ev_imp = importance;
                let event = CharacterEvent {
                    id: Uuid::new_v4().to_string(),
                    tenant_id: tenant_id.to_string(),
                    character_name: name.to_string(),
                    event_name: ev.event_name.chars().take(200).collect(),
                    description: ev.description.chars().take(500).collect(),
                    chapter: ev.chapter,
                    novel: novel_name.to_string(),
                    related_characters: ev.related.clone(),
                    importance: ev_imp,
                    created_at: now_ts,
                    metadata: Metadata::default(),
                };
                self.store.create_event(&event).await.ok();
                stats.events += 1;
            }

            // Death event
            if let (Some(dc), Some(dd)) = (info.death_chapter, info.death_desc.as_ref()) {
                let death_event = CharacterEvent {
                    id: Uuid::new_v4().to_string(),
                    tenant_id: tenant_id.to_string(),
                    character_name: name.to_string(),
                    event_name: format!("第{dc}回 {name}战死/去世"),
                    description: dd.chars().take(500).collect(),
                    chapter: dc,
                    novel: novel_name.to_string(),
                    related_characters: vec![],
                    importance: 1.0,
                    created_at: now_ts,
                    metadata: Metadata::default(),
                };
                self.store.create_event(&death_event).await.ok();
                stats.events += 1;
            }

            // Relations with dimensional scoring.
            //
            // Each relation carries three independent scores in `metadata`,
            // combined into the `importance` field:
            //   - co_occurrence_score: raw frequency / 15 (how often together)
            //   - event_coupling_score: shared events / total events (semantic)
            //   - relation_type_score:  1.0 for typed, 0.3 for generic "关联"
            // Weights: 0.5 / 0.3 / 0.2 — co-occurrence dominates, with bonuses
            // for events that explicitly mention both characters and for
            // relations that match a known keyword pattern.
            for (other, count) in &info.co_occur {
                if *count < 3 {
                    continue;
                }
                let key = if name < other.as_str() {
                    (name.to_string(), other.clone())
                } else {
                    (other.clone(), name.to_string())
                };

                // Skip if the reverse pair was already inserted (bidirectional
                // deduplication): when processing 刘备, the (刘备, 诸葛亮)
                // pair has the same sorted key as (诸葛亮, 刘备) which was
                // inserted earlier.
                if !inserted_pairs.insert(key.clone()) {
                    continue;
                }

                // Prefer the relation type tracked across chapters; fall back
                // to detecting from the stored context (or empty context).
                let rel_type = info
                    .relation_type
                    .get(&key)
                    .cloned()
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| {
                        let ctx = info
                            .relation_text
                            .get(&key)
                            .map(|s| s.as_str())
                            .unwrap_or("");
                        relation::detect_relation_type(ctx, name, other)
                    });
                let rel_chapter = info.relation_chapter.get(&key).copied().unwrap_or(1);

                // === Dimensional scores ===
                let co_occurrence_score = (*count as f64 / 15.0).min(1.0);
                let shared_event_count = info
                    .events
                    .iter()
                    .filter(|e| e.related.iter().any(|r| r == other))
                    .count();
                let event_coupling_score = if info.events.is_empty() {
                    0.0
                } else {
                    (shared_event_count as f64 / info.events.len() as f64).min(1.0)
                };
                let relation_type_score = if rel_type == "关联" { 0.3 } else { 1.0 };

                // Faction bonus: same faction +20%, different -20%, unknown → 1.0
                let faction_bonus = faction::faction_bonus(novel_name, name, other).unwrap_or(1.0);

                let rel_imp = ((0.5 * co_occurrence_score
                    + 0.3 * event_coupling_score
                    + 0.2 * relation_type_score)
                    * faction_bonus)
                    .min(1.0);

                // Apply faction constraint from JSON config (same/any/different)
                let final_importance = match relation::get_faction_constraint(&rel_type) {
                    "same" => {
                        if !faction::same_faction_or_unknown(novel_name, name, other) {
                            rel_imp * 0.2
                        } else {
                            rel_imp
                        }
                    }
                    "different" => {
                        if faction::same_faction_or_unknown(novel_name, name, other) {
                            rel_imp * 0.2
                        } else {
                            rel_imp
                        }
                    }
                    _ => rel_imp,
                };

                // Persist the dimensional breakdown so downstream consumers
                // (MCP tools, network traversal) can explain *why* a relation
                // scored the way it did, not just the combined number.
                let mut metadata = Metadata::default();
                metadata.insert(
                    "co_occurrence_score",
                    serde_json::json!(co_occurrence_score),
                );
                metadata.insert(
                    "event_coupling_score",
                    serde_json::json!(event_coupling_score),
                );
                metadata.insert(
                    "relation_type_score",
                    serde_json::json!(relation_type_score),
                );
                metadata.insert("faction_bonus", serde_json::json!(faction_bonus));
                metadata.insert(
                    "shared_event_count",
                    serde_json::json!(shared_event_count as u64),
                );
                metadata.insert("co_occurrence_count", serde_json::json!(*count as u64));
                metadata.insert("detected_at_chapter", serde_json::json!(rel_chapter));

                let relation = CharacterRelation {
                    id: Uuid::new_v4().to_string(),
                    tenant_id: tenant_id.to_string(),
                    source_character: name.to_string(),
                    target_character: other.clone(),
                    relation_type: rel_type,
                    description: format!("共现{}章", count),
                    chapter: rel_chapter,
                    novel: novel_name.to_string(),
                    bidirections: true,
                    source_type: RelationSource::CoOccurrence,
                    confidence: final_importance,
                    importance: final_importance,
                    created_at: now_ts,
                    metadata,
                };
                self.store.create_relation(&relation).await.ok();
                stats.relations += 1;
            }
        }

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::SQLiteCharacterStore;

    #[test]
    fn ingestion_stats_add_assign() {
        let mut a = IngestionStats {
            characters: 1,
            events: 2,
            relations: 3,
        };
        let b = IngestionStats {
            characters: 10,
            events: 20,
            relations: 30,
        };
        a += b;
        assert_eq!(a.characters, 11);
        assert_eq!(a.events, 22);
        assert_eq!(a.relations, 33);
    }

    #[tokio::test]
    async fn pipeline_handles_empty_corpus_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SQLiteCharacterStore::open_in_memory().await.unwrap());
        let pipeline = IngestionPipeline::new(store, tmp.path().to_str().unwrap());
        let stats = pipeline.run().await;
        // Should get an error or empty stats because no corpus files exist
        assert!(stats.is_err() || stats.unwrap().characters == 0);
    }

    #[tokio::test]
    async fn pipeline_distills_empty_novel() {
        let store = Arc::new(SQLiteCharacterStore::open_in_memory().await.unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = IngestionPipeline::new(store.clone(), tmp.path().to_str().unwrap());
        let stats = pipeline.distill_novel("不存在的小说").await.unwrap();
        assert_eq!(stats.characters, 0);
    }

    #[test]
    fn char_info_tracks_death() {
        let mut info = CharInfo::new(vec!["武松".to_string()]);
        assert!(info.death_chapter.is_none());
        info.death_chapter = Some(117);
        assert_eq!(info.death_chapter, Some(117));
    }

    #[test]
    fn alias_match_sorting() {
        let matches = vec![
            AliasMatch {
                start: 0,
                end: 6,
                alias: "丞相".into(),
                character: "A".into(),
            },
            AliasMatch {
                start: 0,
                end: 9,
                alias: "诸葛亮".into(),
                character: "B".into(),
            },
        ];
        let mut sorted = matches.clone();
        sorted.sort_by_key(|m| (m.start, std::cmp::Reverse(m.alias.len())));
        assert_eq!(sorted[0].alias, "诸葛亮");
        assert_eq!(sorted[1].alias, "丞相");
    }
}
