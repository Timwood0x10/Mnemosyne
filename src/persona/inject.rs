//! Structured persona cards — the "人设不崩" injection layer.
//!
//! This module turns the accumulated `agent_personality`-tagged facts into a
//! stable, structured [`PersonaCard`] that can be dropped straight into a
//! system prompt. It is a **deterministic, no-LLM** aggregation: every field
//! is produced by grouping the stored persona facts by [`FactType`], never by
//! a generative model.
//!
//! Two sources feed a card:
//!
//! - **facts** — [`build_persona_card_from_facts`] aggregates the persisted
//!   `agent_personality` facts for an agent entity (identity / preference /
//!   emotion / goal / relationship).
//! - **JSON file** — an optional, hand-authored `config/persona_cards.json`
//!   (format `{ tenant_id: { agent_id: {identity, persona, style, taboos,
//!   relationship} } }`). File entries take priority and fill in any field the
//!   fact aggregation did not produce (most notably `style` and `taboos`,
//!   which are never derived from facts).
//!
//! The JSON file is optional: [`load_persona_cards`] returns an empty value
//! when the file does not exist, so the tool still works with facts alone.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cognition::{Fact, FactType};
use crate::error::{Error, Result};
use crate::persona::check::filter_persona_facts;

/// Maximum number of persona statements aggregated per facet, so a card never
/// grows unbounded even with a long conversation history.
const MAX_PERSONA_ITEMS: usize = 5;

/// A structured, stable persona card for one (tenant, agent) pair.
///
/// `identity`, `persona`, `style`, `taboos` and `relationship` are the
/// prompt-injectable constraints. `agent_id` / `tenant_id` / `built_at` are
/// metadata kept so a card is self-describing and cacheable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaCard {
    /// The agent identifier this card belongs to.
    pub agent_id: String,
    /// The tenant namespace this card belongs to.
    pub tenant_id: String,
    /// "Who I am" — a single declarative identity line (e.g. "白流苏，离过婚，
    /// 爱过，也输过"). Aggregated from `Identity` facts.
    pub identity: String,
    /// Personality / tone statements, aggregated from `Preference`, `Emotion`
    /// and `Goal` facts (likes, dislikes, emotional leanings, stances).
    pub persona: Vec<String>,
    /// Speaking style notes. Not derived from facts — only supplied by a JSON
    /// persona card.
    pub style: Vec<String>,
    /// Absolute no-go statements. Not derived from facts — only supplied by a
    /// JSON persona card.
    pub taboos: Vec<String>,
    /// Relationship state between the agent and the user. Aggregated from
    /// `Relationship` facts; can be overridden by a JSON card.
    pub relationship: Value,
    /// Epoch seconds when the card was built.
    pub built_at: i64,
}

impl PersonaCard {
    /// Build a card with only metadata and empty content (used as a base
    /// before fact aggregation merges in the persona fields).
    #[must_use]
    fn empty(tenant_id: &str, agent_id: &str) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            tenant_id: tenant_id.to_string(),
            identity: String::new(),
            persona: Vec::new(),
            style: Vec::new(),
            taboos: Vec::new(),
            relationship: Value::Null,
            built_at: now_epoch_secs(),
        }
    }
}

/// Current Unix time in seconds, used for the `built_at` metadata.
#[must_use]
fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Extract the `content` text from a persona fact payload, if present.
fn fact_content(fact: &Fact) -> String {
    fact.payload
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Whether a persona fact payload is marked negated (stance-against).
fn fact_negated(fact: &Fact) -> bool {
    fact.payload
        .get("negated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Deterministically aggregate a structured persona card from the stored
/// `agent_personality`-tagged facts for an agent entity.
///
/// Facts are grouped by [`FactType`]:
///
/// - `Identity` → `identity` (the NEWEST non-negated statement wins, so an
///   evolved identity replaces the origin; falls back to the oldest if only
///   negated ones exist).
/// - `Preference` / `Emotion` / `Goal` → `persona` (the NEWEST
///   [`MAX_PERSONA_ITEMS`] statements — iterating store order kept the five
///   earliest lines forever and discarded everything learned later).
/// - `Relationship` → `relationship` (newest-first, same rationale).
///
/// `style` and `taboos` are never derived from facts and stay empty here.
#[must_use]
pub fn build_persona_card_from_facts(
    tenant_id: &str,
    agent_id: &str,
    facts: &[Fact],
) -> PersonaCard {
    let mut card = PersonaCard::empty(tenant_id, agent_id);

    let mut persona_facts = filter_persona_facts(facts);
    // Store order is time ASC; walk newest-first so the card reflects who the
    // agent has BECOME, not who they were at first contact.
    persona_facts.reverse();

    let mut identity: Option<String> = None;
    let mut identity_oldest: Option<String> = None;
    let mut persona: Vec<String> = Vec::new();
    let mut relationship: Vec<String> = Vec::new();

    for fact in persona_facts {
        let content = fact_content(fact);
        if content.is_empty() {
            continue;
        }
        match fact.fact_type {
            FactType::Identity => {
                if identity_oldest.is_none() {
                    identity_oldest = Some(content.clone());
                }
                // Newest non-negated identity wins (first in reverse order).
                if !fact_negated(fact) && identity.is_none() {
                    identity = Some(content);
                }
            }
            FactType::Preference | FactType::Emotion | FactType::Goal
                if persona.len() < MAX_PERSONA_ITEMS && !persona.contains(&content) =>
            {
                persona.push(content);
            }
            FactType::Relationship
                if relationship.len() < MAX_PERSONA_ITEMS && !relationship.contains(&content) =>
            {
                relationship.push(content);
            }
            _ => {}
        }
    }

    card.identity = identity.or(identity_oldest).unwrap_or_default();
    card.persona = persona;
    card.relationship = if relationship.is_empty() {
        Value::Null
    } else {
        Value::Array(
            relationship
                .into_iter()
                .map(Value::String)
                .collect::<Vec<_>>(),
        )
    };
    card
}

/// Load the persona card JSON file at `path`.
///
/// The expected format is `{ tenant_id: { agent_id: {identity, persona,
/// style, taboos, relationship} } }`. A missing file is **not** an error — it
/// returns an empty object so callers fall back to the fact-aggregated card.
///
/// # Errors
///
/// Returns an I/O error on a non-missing read failure, or a JSON error when
/// the file exists but is malformed.
pub fn load_persona_cards(path: &str) -> Result<Value> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Default::default()));
        }
        Err(e) => return Err(e.into()),
    };
    serde_json::from_str(&content).map_err(Error::from)
}

/// Look up the persona card entry for a `(tenant_id, agent_id)` pair inside a
/// loaded cards value, returning `None` when either key is absent.
#[must_use]
pub fn lookup_persona_card(cards: &Value, tenant_id: &str, agent_id: &str) -> Option<Value> {
    cards
        .get(tenant_id)
        .and_then(|tenant| tenant.get(agent_id))
        .cloned()
}

/// Merge a JSON file entry into a fact-aggregated card.
///
/// Contract (module docs): file entries **fill in** fields the fact
/// aggregation did not produce (`style`, `taboos`, and optionally a pinned
/// `identity`). They must NOT wholesale-replace learned `persona` /
/// `relationship` content — a static card that always wins would freeze the
/// companion at whatever the file said, discarding months of evolved facts
/// on every injection.
#[must_use]
pub fn merge_persona_card_file(mut base: PersonaCard, file: &Value) -> PersonaCard {
    if let Some(s) = file.get("identity").and_then(Value::as_str) {
        if !s.is_empty() {
            base.identity = s.to_string();
        }
    }
    // Persona: UNION file entries with fact-derived ones (file first so a
    // hand-authored line is visible, then the learned statements).
    if let Some(v) = file.get("persona") {
        let items = value_str_array(v);
        if !items.is_empty() {
            let mut merged = items;
            for p in &base.persona {
                if !merged.contains(p) {
                    merged.push(p.clone());
                }
            }
            merged.truncate(MAX_PERSONA_ITEMS * 2);
            base.persona = merged;
        }
    }
    if let Some(v) = file.get("style") {
        let items = value_str_array(v);
        if !items.is_empty() {
            base.style = items;
        }
    }
    if let Some(v) = file.get("taboos") {
        let items = value_str_array(v);
        if !items.is_empty() {
            base.taboos = items;
        }
    }
    // Relationship: MERGE keys rather than replacing the whole value, so a
    // file pinning `status: stranger` cannot erase the learned intimacy/stage.
    if let Some(r) = file.get("relationship") {
        match (&mut base.relationship, r) {
            (Value::Object(base_obj), Value::Object(file_obj)) => {
                for (k, v) in file_obj {
                    base_obj.insert(k.clone(), v.clone());
                }
            }
            // base was Null/non-object: adopt the file object as-is. A null
            // `relationship` is deliberately a no-op, so it falls through to
            // the arm below instead of being spelled out as a dead branch.
            (_, Value::Object(_)) => base.relationship = r.clone(),
            _ => {}
        }
    }
    base
}

/// Coerce a JSON value into a vector of strings (array of strings, or a
/// single string).
fn value_str_array(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Value::String(s) => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// Render a persona card as a text block suitable for pasting into a system
/// prompt. Always includes the `identity` line.
#[must_use]
pub fn persona_card_to_text(card: &PersonaCard) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "[Persona card: {} (tenant {})]\n",
        card.agent_id, card.tenant_id
    ));
    out.push_str(&format!("Identity: {}\n", card.identity));
    if !card.persona.is_empty() {
        out.push_str("Persona:\n");
        for item in &card.persona {
            out.push_str(&format!("- {item}\n"));
        }
    }
    if !card.style.is_empty() {
        out.push_str("Style:\n");
        for item in &card.style {
            out.push_str(&format!("- {item}\n"));
        }
    }
    if !card.taboos.is_empty() {
        out.push_str("Taboos:\n");
        for item in &card.taboos {
            out.push_str(&format!("- {item}\n"));
        }
    }
    if !card.relationship.is_null() {
        out.push_str(&format!("Relationship: {}\n", card.relationship));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_personality::AGENT_PERSONALITY_ATTRIBUTION;

    fn persona_fact(id: i64, fact_type: FactType, negated: bool, content: &str) -> Fact {
        Fact {
            id: Some(id),
            entity_id: 1,
            fact_type,
            time: 1,
            payload: serde_json::json!({
                "attribution": AGENT_PERSONALITY_ATTRIBUTION,
                "content": content,
                "negated": negated,
            }),
            evidence_id: None,
            created_at: 1,
            ..Fact::default()
        }
    }

    /// Objective: Verify facts aggregate into a persona card by type.
    /// Invariants: non-negated identity wins; preference/emotion/goal land in
    /// persona; relationship facts fill the relationship array; style/taboos
    /// stay empty.
    #[test]
    fn aggregates_persona_card_from_facts() {
        let facts = vec![
            persona_fact(
                1,
                FactType::Identity,
                false,
                "我是白流苏，离过婚，爱过，也输过",
            ),
            persona_fact(2, FactType::Preference, false, "我喜欢安稳"),
            persona_fact(3, FactType::Preference, true, "我不喜欢应酬"),
            persona_fact(4, FactType::Emotion, false, "我心里也有害怕的时候"),
            persona_fact(5, FactType::Goal, true, "我不要你为我改什么"),
            persona_fact(
                6,
                FactType::Relationship,
                false,
                "与你相识多年，是我最亲近的人",
            ),
        ];
        let card = build_persona_card_from_facts("tenant-a", "agent-bailiusu", &facts);

        assert_eq!(card.tenant_id, "tenant-a");
        assert_eq!(card.agent_id, "agent-bailiusu");
        assert_eq!(
            card.identity, "我是白流苏，离过婚，爱过，也输过",
            "non-negated identity wins"
        );
        assert_eq!(card.persona.len(), 4, "preference+emotion+goal → persona");
        assert!(card.persona.iter().any(|s| s.contains("我喜欢安稳")));
        assert!(card.persona.iter().any(|s| s.contains("我不喜欢应酬")));
        assert_eq!(
            card.relationship.as_array().map(|a| a.len()),
            Some(1),
            "one relationship fact → one relationship entry"
        );
        assert!(card.style.is_empty(), "style is never derived from facts");
        assert!(
            card.taboos.is_empty(),
            "taboos are never derived from facts"
        );
    }

    /// Objective: Verify a negated identity fact is used as a fallback when no
    /// non-negated identity exists.
    /// Invariants: only a negated identity → identity still set to it.
    #[test]
    fn negated_identity_is_fallback() {
        let facts = vec![persona_fact(
            1,
            FactType::Identity,
            true,
            "我不再是那个天真的人",
        )];
        let card = build_persona_card_from_facts("default", "agent-x", &facts);
        assert_eq!(
            card.identity, "我不再是那个天真的人",
            "negated identity used when no affirmative one exists"
        );
    }

    /// Objective: Verify the persona list is capped at `MAX_PERSONA_ITEMS`.
    /// Invariants: 10 preference facts → at most 5 persona entries.
    #[test]
    fn persona_list_is_bounded() {
        let facts: Vec<Fact> = (0..10)
            .map(|i| persona_fact(i, FactType::Preference, false, &format!("喜欢话题{i}")))
            .collect();
        let card = build_persona_card_from_facts("default", "agent-x", &facts);
        assert_eq!(card.persona.len(), MAX_PERSONA_ITEMS, "persona is capped");
    }

    /// Objective: Verify a JSON file entry fills missing fields and overrides
    /// present ones, with missing fields falling back to the aggregated card.
    /// Invariants: file identity/style/taboos win; file persona wins; file
    /// relationship wins; absent fields keep aggregated values.
    #[test]
    fn json_card_fills_missing_fields_and_overrides() {
        let facts = vec![
            persona_fact(1, FactType::Identity, false, "我是白流苏"),
            persona_fact(2, FactType::Preference, false, "我喜欢安稳"),
        ];
        let base = build_persona_card_from_facts("tenant-a", "agent-bailiusu", &facts);

        let file = serde_json::json!({
            "identity": "白流苏，离过婚，爱过，也输过",
            "style": ["话少，克制，偶尔揶揄"],
            "taboos": ["绝不说自己已经放下"],
            "relationship": {"status": "彼此揣着明白"}
        });
        let merged = merge_persona_card_file(base, &file);

        assert_eq!(
            merged.identity, "白流苏，离过婚，爱过，也输过",
            "file identity wins"
        );
        assert_eq!(merged.style, vec!["话少，克制，偶尔揶揄"]);
        assert_eq!(merged.taboos, vec!["绝不说自己已经放下"]);
        assert_eq!(merged.relationship["status"], "彼此揣着明白");
        assert_eq!(
            merged.persona,
            vec!["我喜欢安稳"],
            "absent file persona falls back to aggregated facts"
        );
    }

    /// Objective: Verify a missing persona card file loads as an empty value
    /// without erroring, and lookup returns None.
    /// Invariants: non-existent path → Ok(empty), lookup → None.
    #[test]
    fn missing_file_loads_empty() {
        let path = "/nonexistent/persona_cards.json";
        let cards = load_persona_cards(path).expect("missing file must not error");
        assert!(
            lookup_persona_card(&cards, "default", "agent-x").is_none(),
            "no entry in an empty card set"
        );
    }

    /// Objective: Verify `persona_card_to_text` always includes the identity.
    /// Invariants: text output contains the identity line.
    #[test]
    fn text_output_contains_identity() {
        let facts = vec![persona_fact(1, FactType::Identity, false, "我是白流苏")];
        let card = build_persona_card_from_facts("default", "agent", &facts);
        let text = persona_card_to_text(&card);
        assert!(text.contains("我是白流苏"), "text includes identity");
        assert!(text.contains("agent"), "text includes the agent id");
    }
}
