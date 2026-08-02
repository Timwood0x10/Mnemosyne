//! Profile Extractor — Pass 1: World Builder.
//!
//! Extracts Entity Profile attributes from character introduction sections:
//!
//! ```text
//! "刘备字玄德，涿郡涿县人，中山靖王之后"
//!   → Entity("刘备") + Profile(courtesy_name="玄德") + Profile(birthplace="涿郡")
//! ```
//!
//! Supports both Chinese classical patterns (字, 人也, 身长...) and arbitrary
//! per-novel patterns loaded from JSON config (e.g. "was the son of", "married").

use crate::compiler::entity::EntityDictionary;
use crate::compiler::{CompileContext, Entity, EntityProfile};
use crate::language::LanguageProvider;
use serde::Deserialize;

/// A single profile extraction pattern loaded from JSON config.
///
/// Each pattern defines a substring to match in the text, a profile key to
/// store the extracted value under, and an extraction mode that determines
/// how the value is extracted relative to the matched pattern.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfilePattern {
    pub pattern: String,
    pub key: String,
    pub mode: String, // "After", "Before", "Between", "Until"
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub fallback: Option<String>,
}

impl ProfilePattern {
    /// Convert the string mode to an [`ExtractMode`].
    /// JSON patterns use static defaults for suffix/fallback since
    /// [`ExtractMode`] stores `&'static str` references.
    pub(crate) fn to_extract_mode(&self) -> ExtractMode {
        match self.mode.as_str() {
            "After" => ExtractMode::After,
            "Before" => ExtractMode::Before,
            "Between" => ExtractMode::Between(""),
            "Until" => ExtractMode::Until("，"),
            _ => ExtractMode::After,
        }
    }
}

/// Default profile patterns with their key names (classical Chinese novel format).
/// Used when no `LanguageProvider` override is provided.
pub(crate) const DEFAULT_PROFILE_PATTERNS: &[(&str, &str, ExtractMode)] = &[
    // (pattern, profile_key, extraction_mode)
    ("字", "courtesy_name", ExtractMode::After),
    ("人也", "birthplace", ExtractMode::Before),
    ("之后", "ancestry", ExtractMode::Before),
    ("身长", "appearance_height", ExtractMode::Between("尺")),
    ("面如", "appearance_face", ExtractMode::After),
    ("为业", "occupation", ExtractMode::Before),
    ("使", "weapon", ExtractMode::Until("，")),
    ("姓", "surname", ExtractMode::Between("名")),
    ("名", "given_name", ExtractMode::BeforeWithFallback("字")),
    ("号", "title", ExtractMode::After),
    ("威风", "demeanor", ExtractMode::After),
];

pub(crate) enum ExtractMode {
    /// Extract text after the pattern, stopping at stop chars.
    After,
    /// Extract text before the pattern, taking the last segment before a stop.
    Before,
    /// Extract text after `prefix` and before `suffix`.
    Between(&'static str),
    /// Extract text after the pattern until a stop char.
    Until(&'static str),
    /// Extract text before the pattern; if empty, fall back to scanning for
    /// another pattern as boundary.
    BeforeWithFallback(&'static str),
}

/// Characters that mark the end of a profile value.
fn is_stop(c: char) -> bool {
    matches!(c, '，' | ',' | '。' | '；' | '、' | '\n' | '：')
}

/// Extract entity profiles from text.
///
/// When `dict` is `Some`, known entity names are validated against the dictionary
/// and aliases are resolved to canonical names. When `dict` is `None`, entity
/// names are discovered heuristically from introduction patterns.
pub fn extract_profiles(
    text: &str,
    ctx: &mut CompileContext,
    dict: Option<&EntityDictionary>,
    extra_patterns: &[ProfilePattern],
    lang: &dyn LanguageProvider,
) {
    for line in text.lines() {
        let line = line.trim();
        if line.len() < 6 {
            continue;
        }

        // Try dictionary-based entity lookup first, then heuristic discovery.
        let dictionary_entity = dict
            .and_then(|dictionary| find_entity_in_text(line, dictionary))
            .map(|(name, _)| name);
        // Entity-name validation happens INSIDE discover_entity_name's Chinese
        // branch (English title names like "Mr. Bennet" bypass it); dictionary
        // hits are trusted and skip it — 三国/水浒 with a known lexicon keep
        // their entities.
        let discovered_entity = dictionary_entity
            .is_none()
            .then(|| discover_entity_name(line, lang.discovery_markers()))
            .flatten();
        let entity_name = dictionary_entity.or_else(|| discovered_entity.clone());

        let Some(entity_name) = entity_name else {
            continue;
        };

        // Extract profile attributes using configured or default patterns.
        let mut profiles: Vec<(&str, String)> = Vec::new();
        if let Some(title) = discovered_entity
            .as_deref()
            .and_then(|name| discover_title(name, lang.discovery_markers()))
        {
            profiles.push(("title", title.to_string()));
        }

        // Pattern source 1: JSON-configured patterns
        for pp in extra_patterns {
            if line.contains(&pp.pattern) {
                let mode = pp.to_extract_mode();
                let val = match &mode {
                    ExtractMode::After => extract_after(line, &pp.pattern),
                    ExtractMode::Before => extract_before(line, &pp.pattern),
                    ExtractMode::Between(suffix) => extract_between(line, &pp.pattern, suffix),
                    ExtractMode::Until(stop) => extract_until(line, &pp.pattern, stop),
                    ExtractMode::BeforeWithFallback(fallback) => {
                        extract_before(line, &pp.pattern).or_else(|| extract_before(line, fallback))
                    }
                };
                if let Some(v) = val {
                    profiles.push((pp.key.as_str(), v));
                }
            }
        }

        // Pattern source 2: language frontend definitions.
        for definition in lang.profile_patterns() {
            if line.contains(definition.pattern) {
                let value = if definition.key == "title" {
                    Some(definition.pattern.to_string())
                } else {
                    match definition.mode {
                        "After" => extract_after(line, definition.pattern),
                        "Before" => extract_before(line, definition.pattern),
                        "Between" => definition
                            .suffix
                            .and_then(|suffix| extract_between(line, definition.pattern, suffix)),
                        "Until" => definition
                            .suffix
                            .and_then(|stop| extract_until(line, definition.pattern, stop)),
                        "BeforeWithFallback" => {
                            extract_before(line, definition.pattern).or_else(|| {
                                definition
                                    .suffix
                                    .and_then(|fallback| extract_before(line, fallback))
                            })
                        }
                        _ => None,
                    }
                };
                if let Some(value) = value {
                    profiles.push((definition.key, value));
                }
            }
        }

        // Pattern source 3: legacy defaults retained for Chinese compatibility.
        for &(pattern, key, ref mode) in DEFAULT_PROFILE_PATTERNS {
            if line.contains(pattern) {
                let val = match mode {
                    ExtractMode::After => extract_after(line, pattern),
                    ExtractMode::Before => extract_before(line, pattern),
                    ExtractMode::Between(suffix) => extract_between(line, pattern, suffix),
                    ExtractMode::Until(stop) => extract_until(line, pattern, stop),
                    ExtractMode::BeforeWithFallback(fallback) => {
                        extract_before(line, pattern).or_else(|| extract_before(line, fallback))
                    }
                };
                if let Some(v) = val {
                    if !profiles.iter().any(|(k, _)| *k == key && v.contains(k)) {
                        profiles.push((key, v));
                    }
                }
            }
        }

        if profiles.is_empty() {
            continue;
        }

        // Create Entity (if not already in ctx)
        let name = entity_name.to_string();
        if !ctx.entities.iter().any(|e| e.name == name) {
            ctx.entities.push(Entity {
                id: None,
                name: name.clone(),
                entity_type: "person".into(),
                status: "active".into(),
                importance: 0.5,
            });
        }

        // Assign a synthetic ID for linking profiles to entities.
        // Write it back to Entity.id so downstream code (e.g.
        // register_discovered_entities) can match profiles to entities.
        let eid = ctx
            .entities
            .iter()
            .position(|e| e.name == name)
            .map(|i| (i + 1) as i64);
        if let Some(pos) = ctx.entities.iter().position(|e| e.name == name) {
            ctx.entities[pos].id = eid;
        }

        // Create Profiles
        for (key, value) in &profiles {
            if !ctx
                .profiles
                .iter()
                .any(|p| p.entity_id == eid && p.key == *key)
            {
                ctx.profiles.push(EntityProfile {
                    entity_id: eid,
                    key: key.to_string(),
                    value: value.clone(),
                    confidence: 0.9,
                });
            }
        }
    }
}

/// Wire Pass 1's discovered entities into the dictionary so Pass 2 (Story
/// Compiler) can resolve mentions of those entities and their aliases.
///
/// This MUST be called after [`extract_profiles`] and before
/// [`extract::compile`](crate::compiler::extract::compile). Without this
/// step, entities discovered heuristically by Pass 1 are invisible to
/// Pass 2 — the classic "un-wired pipeline" symptom where Pass 1 finds
/// hundreds of entities but Pass 2 extracts zero events.
///
/// For each entity in `ctx.entities`, the entity's canonical name is
/// registered, plus any profile values whose key is `courtesy_name` or
/// `title` (these serve as aliases — e.g. "玄德" → "刘备").
pub fn register_discovered_entities(dict: &mut EntityDictionary, ctx: &CompileContext) {
    // Start synthetic IDs from 10000 to avoid conflicts with real DB IDs.
    let mut next_id = 10000i64;
    // Seed with the max existing ID if any were pre-assigned.
    for &id in dict.name_to_id.values() {
        if id >= next_id {
            next_id = id + 1;
        }
    }
    // Also check existing alias_to_canonical keys that might have IDs.
    // Reset the counter to a safe offset.
    if next_id < 10000 {
        next_id = 10000;
    }

    for entity in &ctx.entities {
        let aliases: Vec<&str> = ctx
            .profiles
            .iter()
            .filter(|p| p.entity_id == entity.id)
            .filter(|p| p.key == "courtesy_name" || p.key == "title")
            .map(|p| p.value.as_str())
            .collect();
        // Only register if not already in the dictionary
        if !dict.name_to_id.contains_key(&entity.name) {
            dict.register_discovered(&entity.name, &aliases);
            dict.name_to_id.insert(entity.name.clone(), next_id);
            next_id += 1;
        }
    }
}

/// Find the first known entity (by canonical name or alias) in the text.
fn find_entity_in_text(text: &str, dict: &EntityDictionary) -> Option<(String, Option<i64>)> {
    let mut candidates: Vec<&String> = dict.alias_to_canonical.keys().collect();
    // Sort by length DESCENDING (longest match first), then alphabetically as a
    // deterministic tie-breaker. Without the secondary key, same-length aliases
    // (e.g. "刘备" and "玄德", both 2 chars) would resolve in HashMap iteration
    // order, making entity extraction non-reproducible across builds.
    candidates.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));

    for alias in candidates {
        if text.contains(alias.as_str()) {
            return dict.resolve(alias.as_str());
        }
    }
    None
}

/// Heuristically discover entity name from a profile line.
///
/// Uses the provided `markers` to find introduction patterns.
/// Chinese markers: "字", "者也", "身长", "面如", ...
/// English markers: "Prince", "Count", "Mr.", ...
fn discover_entity_name(line: &str, markers: &[&str]) -> Option<String> {
    if let Some(name) = discover_english_title_name(line, markers) {
        return Some(name);
    }

    // Explicit noise words — function words that are never entity names.
    const NOISE: &[&str] = &[
        "不", "来", "一", "而", "有", "乃", "二", "后", "自", "可", "皆", "之", "以", "其", "此",
        "何", "与", "也", "矣", "乎", "所", "能", "欲", "遂", "即", "便", "则", "且", "若", "焉",
        "耳", "无", "非", "岂", "诚", "或", "既", "故", "盖", "又", "是", "为", "于", "因", "当",
        "已", "先", "长", "对", "进", "天", "再", "见", "时", "却", "正", "忽", "四", "尽", "闻",
        "蜀", "方", "大", "知", "至", "欲", "如", "实", "但", "别", "应", "从", "出", "各", "每",
        "共", "同", "向", "引", "带", "随", "百", "手", "某", "听", "帝", "国", "安", "半", "必",
        "常", "道", "德", "多", "法", "凡", "反", "奉", "告", "功", "光", "归", "果", "好", "号",
        "回", "会", "计", "加", "交", "教", "今", "近", "久", "就", "举", "决", "军", "开", "口",
        "立", "利", "连", "两", "满", "门", "明", "难", "内", "年", "平", "七", "岂", "起", "强",
        "亲", "请", "秋", "全", "任", "日", "如", "入", "少", "设", "深", "甚", "生", "师", "十",
        "始", "事", "受", "数", "帅", "思", "四", "通", "同", "头", "外", "完", "万", "望", "未",
        "问", "五", "相", "孝", "心", "行", "形", "修", "言", "阳", "依", "意", "英", "永", "用",
        "由", "友", "余", "欲", "元", "远", "愿", "月", "云", "再", "早", "战", "朝", "真", "正",
        "政", "终", "重", "主", "住", "转", "开", "历", "众", "种", "足", "最", "备", "鄙", "毕",
        "变", "表", "并", "步", "策", "参", "昌", "常", "臣", "称", "成", "诚", "处", "传", "辞",
        "此", "次", "从", "存", "达", "代", "单", "到", "弟", "典", "定", "独", "度", "段", "对",
        "夺", "额", "发", "法", "反", "方", "非", "分", "否", "夫", "服", "付", "付", "副", "盖",
        "敢", "刚", "高", "告", "哥", "各", "更", "工", "公", "功", "共", "故", "顾", "怪", "官",
        "光", "广", "归", "贵", "国", "果", "过", "还", "好", "何", "合", "恨", "后", "忽", "化",
        "话", "怀", "还", "皇", "回", "会", "活", "或", "极", "急", "计", "记", "季", "加", "家",
        "间", "见", "将", "交", "皆", "接", "节", "结", "今", "金", "尽", "近", "经", "惊", "精",
        "景", "竟", "敬", "久", "旧", "举", "具", "据", "觉", "军", "开", "看", "科", "可", "空",
        "口", "苦", "快", "来", "老", "乐", "累", "类", "离", "礼", "李", "里", "力", "立", "利",
        "连", "联", "两", "量", "了", "临", "令", "流", "路", "论", "落", "旅", "麻", "马", "满",
        "没", "每", "门", "们", "面", "民", "名", "明", "命", "某", "母", "目", "那", "难", "年",
        "宁", "牛", "女", "怕", "旁", "朋", "批", "偏", "平", "评", "破", "期", "其", "奇", "起",
        "气", "千", "前", "强", "且", "亲", "青", "轻", "清", "情", "请", "穷", "秋", "去", "全",
        "任", "入", "三", "杀", "山", "上", "少", "社", "设", "身", "深", "神", "生", "声", "师",
        "十", "时", "食", "实", "始", "事", "试", "是", "室", "收", "手", "首", "受", "书", "术",
        "数", "双", "谁", "水", "睡", "顺", "说", "私", "死", "四", "送", "诉", "速", "岁", "他",
        "太", "谈", "特", "提", "体", "天", "田", "听", "同", "头", "突", "图", "推", "退", "外",
        "完", "万", "往", "望", "微", "为", "文", "闻", "问", "我", "无", "五", "习", "细", "下",
        "先", "显", "现", "相", "想", "向", "像", "效", "心", "信", "行", "形", "幸", "性", "休",
        "修", "须", "许", "选", "学", "血", "言", "阳", "样", "药", "也", "业", "叶", "夜", "一",
        "医", "依", "疑", "已", "以", "意", "义", "因", "阴", "应", "英", "营", "影", "永", "用",
        "由", "友", "有", "又", "于", "余", "鱼", "与", "语", "元", "原", "远", "院", "愿", "月",
        "云", "运", "再", "早", "则", "怎", "增", "展", "站", "章", "长", "丈", "者", "正", "政",
        "知", "之", "职", "止", "只", "至", "志", "制", "治", "中", "终", "重", "众", "周", "主",
        "住", "注", "抓", "专", "转", "准", "子", "字", "自", "走", "足", "族", "组", "最", "昨",
        "左", "作", "坐", "座", "恐",
    ];

    // Surname/given-name markers act as hard stop boundaries so the walker
    // never crosses them (e.g. for `姓刘名备，字玄德` with marker `字`, the text
    // before is `姓刘名备，` — stopping at `名` yields `备` instead of the old
    // garbage `名备` that spanned the given-name marker).
    const STOP_BOUNDARIES: &[char] = &['名', '姓'];
    for marker in markers {
        if let Some(pos) = line.find(marker) {
            let before = &line[..pos];
            let mut result = String::new();
            for c in before.chars().rev() {
                // Use CHARACTER count, not byte length — each CJK char is 3
                // bytes in UTF-8, so the old `result.len() >= 4` broke after
                // just 2 chars (6 bytes), producing garbage like `名备`.
                if result.chars().count() >= 4 {
                    break;
                }
                if STOP_BOUNDARIES.contains(&c) {
                    // Hit a surname/given-name marker — stop walking.
                    break;
                }
                if ('\u{4e00}'..='\u{9fff}').contains(&c) {
                    result.insert(0, c);
                } else if c == '，' || c == ',' || c == '、' || c == ' ' {
                    // Skip light punctuation between name and marker (commas,
                    // enumeration marks, spaces). `。` is NOT skipped — it's a
                    // sentence boundary, not a name/marker separator. Skipping
                    // it would walk across sentences and produce garbage like
                    // "说话刘备" from "话说。刘备字玄德".
                    continue;
                } else {
                    break;
                }
            }
            if result.chars().count() >= 2
                && result.chars().count() <= 4
                && !NOISE.contains(&result.as_str())
                && !result.contains("侧放")
                && !result.contains("书一行")
                && !result.contains("上系")
                && !result.contains("相连")
                && !result.contains("尚幼")
                && !result.contains("皆与")
                && !result.contains("太守")
                && !result.contains("却有")
                && !result.contains("篆文")
                && !result.contains("锦绣")
                && is_valid_person_name(&result)
            {
                return Some(result);
            }
        }
    }
    None
}

/// Validate that a heuristically discovered entity name looks like a real
/// person name (P-constraint). Tables live in `config/name_validation.json`
/// (user-extensible) — see [`crate::compiler::name_validation`].
use crate::compiler::name_validation::is_valid_person_name;

/// Discover an English personal name beginning with a configured title.
fn discover_english_title_name(line: &str, markers: &[&str]) -> Option<String> {
    if is_gutenberg_metadata(line) {
        return None;
    }

    // P3: prefer the lexicon-driven title matcher (single-pass Aho-Corasick
    // over the `title` semantic class) when it has patterns; fall back to the
    // language provider's marker list for titles not yet in the lexicon.
    static TITLE_MATCHER: std::sync::LazyLock<crate::lexicon::LexiconMatcher> =
        std::sync::LazyLock::new(|| {
            crate::lexicon::LexiconMatcher::from_global_registry_class("title")
        });

    let mut marker_hits: Vec<(usize, &str)> = Vec::new();
    for m in TITLE_MATCHER.find_iter(line) {
        marker_hits.push((m.start, &line[m.start..m.end]));
    }
    // Fall back to provider markers (dedup by position).
    for marker in markers {
        if let Some(position) = find_title_boundary(line, marker) {
            if !marker_hits.iter().any(|(p, _)| *p == position) {
                marker_hits.push((position, marker));
            }
        }
    }
    marker_hits.sort_by_key(|(p, _)| *p);

    for (position, _marker) in marker_hits {
        // Slice from the marker START so the title itself ("Mr.") is the first
        // token counted toward the ≥2 word requirement for a full name.
        let after = &line[position..];
        let words: Vec<String> = after
            .split_whitespace()
            .take(4)
            .map(normalize_english_name_word)
            .take_while(|word| {
                word.chars().next().is_some_and(char::is_uppercase)
                    && !crate::dictionary::is_english_stop_name(word)
            })
            .collect();
        if words.len() < 2 {
            continue;
        }
        let name = words.join(" ");
        if name.split_whitespace().count() >= 2 {
            return Some(name);
        }
    }
    None
}

/// Preserve a configured title while trimming punctuation from name words.
fn normalize_english_name_word(word: &str) -> String {
    // Strip surrounding non-alphabetic characters (quotes, commas, etc.).
    let word = word.trim_matches(|character: char| !character.is_alphabetic());
    match word {
        "Mr" => "Mr.".to_string(),
        "Mrs" => "Mrs.".to_string(),
        "Dr" => "Dr.".to_string(),
        _ => {
            // Take only the leading alphabetic segment, stopping at any
            // interior punctuation (dash, comma, quote, etc.) so that
            // "Andrew—and" → "Andrew" and "Well," → "Well".
            word.chars()
                .take_while(|&c| c.is_alphabetic() || c == '.')
                .collect::<String>()
        }
    }
}

/// Require title markers to start at a word boundary.
fn find_title_boundary(line: &str, marker: &str) -> Option<usize> {
    line.match_indices(marker).find_map(|(position, _)| {
        let before_is_boundary = line[..position]
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_alphabetic());
        let after_position = position + marker.len();
        let after_is_boundary = line[after_position..]
            .chars()
            .next()
            .is_none_or(|character| !character.is_alphabetic());
        (before_is_boundary && after_is_boundary).then_some(position)
    })
}

/// Return the title prefix from a normalized discovered entity name.
fn discover_title<'a>(name: &str, markers: &'a [&str]) -> Option<&'a str> {
    markers.iter().copied().find(|marker| {
        name.strip_prefix(marker)
            .is_some_and(|suffix| suffix.starts_with(char::is_whitespace))
    })
}

/// Exclude Project Gutenberg administrative text from literary entity discovery.
fn is_gutenberg_metadata(line: &str) -> bool {
    const MARKERS: &[&str] = &[
        "Project Gutenberg",
        "Gutenberg-tm",
        "Full Project Gutenberg",
        "General Terms of Use",
        "General Information About Project Gutenberg",
    ];
    MARKERS.iter().any(|marker| line.contains(marker))
}

/// Extract text after a prefix pattern, stopping at the first stop character.
fn extract_after(line: &str, prefix: &str) -> Option<String> {
    let start = line.find(prefix)?;
    let after = &line[start + prefix.len()..];
    let value: String = after.chars().take_while(|c| !is_stop(*c)).collect();
    if value.is_empty() { None } else { Some(value) }
}

/// Extract text before a suffix pattern, taking the last segment.
fn extract_before(line: &str, suffix: &str) -> Option<String> {
    let end = line.find(suffix)?;
    let before = &line[..end];
    let value: String = before
        .chars()
        .rev()
        .take_while(|c| !is_stop(*c))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if value.is_empty() { None } else { Some(value) }
}

/// Extract text between prefix and suffix.
fn extract_between(line: &str, prefix: &str, suffix: &str) -> Option<String> {
    let start = line.find(prefix)?;
    let after = &line[start + prefix.len()..];
    let end = after.find(suffix)?;
    Some(after[..end].to_string())
}

/// Extract text after pattern until stop.
fn extract_until(line: &str, pattern: &str, stop: &str) -> Option<String> {
    let start = line.find(pattern)?;
    let after = &line[start + pattern.len()..];
    let end = after.find(stop).unwrap_or(after.len());
    Some(after[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dict() -> EntityDictionary {
        let mut d = EntityDictionary::default();
        d.alias_to_canonical.insert("刘备".into(), "刘备".into());
        d.alias_to_canonical.insert("玄德".into(), "刘备".into());
        d.alias_to_canonical.insert("关羽".into(), "关羽".into());
        d.alias_to_canonical.insert("云长".into(), "关羽".into());
        d.alias_to_canonical.insert("张飞".into(), "张飞".into());
        d
    }

    /// Objective: Verify the person-name validator accepts real names and
    /// rejects the garbage the heuristic produced on 大秦帝国.
    /// Invariants: common names + compound surnames pass; function-word/noun
    /// tails / non-surname heads / bad shapes are rejected.
    #[test]
    fn person_name_validation_gates() {
        // C1: shape.
        assert!(!is_valid_person_name("嬴"), "single char is not a name");
        assert!(!is_valid_person_name("嬴渠梁一"), "5 chars rejected");
        assert!(!is_valid_person_name("abc"), "non-CJK rejected");

        // C2: surname-led (incl. compound).
        assert!(is_valid_person_name("嬴渠梁"), "嬴 is a surname");
        assert!(is_valid_person_name("商鞅"), "商 is a surname");
        assert!(is_valid_person_name("吕不韦"), "吕 is a surname");
        assert!(is_valid_person_name("公孙鞅"), "compound surname 公孙");
        assert!(is_valid_person_name("司马错"), "compound surname 司马");
        assert!(!is_valid_person_name("涓的秘"), "涓 is not a surname");

        // C3: function words anywhere.
        assert!(!is_valid_person_name("涓的秘密"), "ends with 的");
        assert!(!is_valid_person_name("的感觉却"), "ends with 却");
        assert!(
            !is_valid_person_name("一金令"),
            "starts with function word 一"
        );

        // C4: noun tails.
        assert!(!is_valid_person_name("牛角"), "ends with noun 角");
        assert!(!is_valid_person_name("白绢衣裤"), "ends with noun 裤");
        assert!(!is_valid_person_name("一金令箭"), "ends with noun 箭");
        assert!(!is_valid_person_name("文明时代"), "ends with noun 代");

        // Sanity: real person with a noun-looking char in the MIDDLE still
        // passes (only the TAIL is gated by C4). `王金城` would be rejected
        // because 城 is a noun tail — that is the intended C4 behavior.
        assert!(
            is_valid_person_name("李金诚"),
            "name containing 金 mid-name is fine"
        );
    }

    /// Objective: Verify the heuristic garbage from the 大秦帝国 baseline is
    /// now filtered by the constraint inside `extract_profiles`.
    /// Invariants: a line that used to yield `涓的秘密`-style entities now
    /// yields none, while real introduction lines still extract.
    #[test]
    fn validation_filters_baseline_garbage() {
        let lang = crate::language::ChineseLanguageProvider::new();
        let mut ctx = CompileContext {
            document_title: "test".into(),
            ..Default::default()
        };
        // Garbage-prone line: marker 也 preceded by a non-name fragment.
        profile_lines_garbage(&mut ctx, &lang);
        assert!(
            ctx.entities.iter().all(|e| is_valid_person_name(&e.name)),
            "all extracted entities must pass the name validator"
        );
    }

    /// Feed lines that previously produced garbage entities and assert no
    /// invalid entity survives.
    fn profile_lines_garbage(
        ctx: &mut CompileContext,
        lang: &crate::language::ChineseLanguageProvider,
    ) {
        // These are the kind of fragments the baseline mis-discovered.
        for line in [
            "涓的秘密者也。",
            "一金令箭者也。",
            "白绢衣裤者也。",
            "牛角者也。",
        ] {
            extract_profiles(line, ctx, None, &[], lang);
        }
        assert!(
            ctx.entities.is_empty(),
            "garbage fragments must yield no entities, got {:?}",
            ctx.entities
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
        );
    }

    /// Objective: Verify that "字玄德" after "刘备" extracts courtesy_name.
    /// Invariants: Profile key "courtesy_name" with value "玄德".
    #[test]
    fn courtesy_from_dialog() {
        let mut ctx = CompileContext::default();
        extract_profiles(
            "刘备字玄德，涿郡人也",
            &mut ctx,
            Some(&make_dict()),
            &[],
            &crate::language::ChineseLanguageProvider::new(),
        );
        let cp = ctx.profiles.iter().find(|p| p.key == "courtesy_name");
        assert!(cp.is_some(), "courtesy_name should be extracted");
        assert_eq!(cp.unwrap().value, "玄德");
        assert!(
            ctx.entities.iter().any(|e| e.name == "刘备"),
            "刘备 entity created"
        );
    }

    /// Objective: Verify that birthplace is extracted from "XX人也".
    /// Invariants: Profile key "birthplace" with the region name.
    #[test]
    fn birthplace_extracted() {
        let mut ctx = CompileContext::default();
        extract_profiles(
            "张飞涿郡人也",
            &mut ctx,
            Some(&make_dict()),
            &[],
            &crate::language::ChineseLanguageProvider::new(),
        );
        let bp = ctx.profiles.iter().find(|p| p.key == "birthplace");
        assert!(bp.is_some(), "birthplace should be extracted");
        assert!(bp.unwrap().value.contains("涿郡"));
    }

    /// Objective: Verify that weapon is extracted from "使XX".
    /// Invariants: Profile key "weapon" with the weapon name.
    #[test]
    fn weapon_extracted() {
        let mut ctx = CompileContext::default();
        extract_profiles(
            "关羽使青龙偃月刀",
            &mut ctx,
            Some(&make_dict()),
            &[],
            &crate::language::ChineseLanguageProvider::new(),
        );
        let wp = ctx.profiles.iter().find(|p| p.key == "weapon");
        assert!(wp.is_some(), "weapon should be extracted");
        assert_eq!(wp.unwrap().value, "青龙偃月刀");
    }

    /// Objective: Verify that narrative text without entity names produces nothing.
    /// Invariants: No entities or profiles created.
    #[test]
    fn narrative_text_ignored() {
        let mut ctx = CompileContext::default();
        extract_profiles(
            "话说天下大势，分久必合",
            &mut ctx,
            Some(&make_dict()),
            &[],
            &crate::language::ChineseLanguageProvider::new(),
        );
        assert!(ctx.entities.is_empty(), "no entity for narrative text");
        assert!(ctx.profiles.is_empty(), "no profiles for narrative text");
    }

    /// Objective: Verify that alias mention ("玄德") resolves to canonical name ("刘备").
    /// Invariants: Entity created with name "刘备", not "玄德".
    #[test]
    fn alias_resolves_to_canonical() {
        let mut ctx = CompileContext::default();
        extract_profiles(
            "玄德幼孤，事母至孝",
            &mut ctx,
            Some(&make_dict()),
            &[],
            &crate::language::ChineseLanguageProvider::new(),
        );
        for entity in &ctx.entities {
            assert_ne!(
                entity.name, "玄德",
                "Entity names should be canonical rather than aliases"
            );
        }
    }

    /// Objective: Verify that `。` acts as a sentence boundary and is never
    /// skipped when walking back to a name (NEW-H24 regression lock).
    /// Invariants: "话说。刘备字玄德" must discover "刘备" — never the garbage
    /// "说话刘备" that would result from crossing the `。` boundary.
    #[test]
    fn sentence_boundary_stops_name_walk() {
        let mut ctx = CompileContext::default();
        extract_profiles(
            "话说。刘备字玄德，涿郡人也。",
            &mut ctx,
            Some(&make_dict()),
            &[],
            &crate::language::ChineseLanguageProvider::new(),
        );
        let names: Vec<&str> = ctx.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"刘备"),
            "`。`-boundary walk must still find 刘备, got {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| n.contains("说话") || n.contains("话说")),
            "name walk must not cross the `。` sentence boundary, got {names:?}"
        );
    }

    /// Objective: Verify `find_entity_in_text` is deterministic for same-length
    /// aliases (NEW-H25 regression lock) — HashMap iteration order must not
    /// leak into resolution.
    /// Invariants: Repeated resolution of text containing two same-length
    /// aliases yields the same entity every time.
    #[test]
    fn same_length_alias_resolution_is_deterministic() {
        let dict = make_dict();
        let mut results = std::collections::HashSet::new();
        for _ in 0..50 {
            let hit = find_entity_in_text("关羽字云长", &dict);
            results.insert(hit.map(|(name, _)| name));
        }
        assert_eq!(
            results.len(),
            1,
            "same-length alias resolution must be stable across runs, got {results:?}"
        );
    }

    /// Objective: Verify the English frontend discovers titled personal names.
    /// Invariants: The entity retains title plus name and a title profile.
    #[test]
    fn english_title_discovers_person_entity() {
        let mut context = CompileContext::default();
        extract_profiles(
            "Prince Andrei Bolkonsky was the son of Prince Nikolai Bolkonsky.",
            &mut context,
            None,
            &[],
            &crate::language::EnglishLanguageProvider::new(),
        );

        assert!(
            context
                .entities
                .iter()
                .any(|entity| entity.name == "Prince Andrei Bolkonsky"),
            "English title discovery should preserve the titled personal name"
        );
        assert!(
            context
                .profiles
                .iter()
                .any(|profile| profile.key == "title" && profile.value == "Prince"),
            "English title discovery should emit a title profile"
        );
    }

    /// Objective: Verify dotted English honorifics survive normalization.
    /// Invariants: Each discovered entity preserves its canonical dotted title and profile.
    #[test]
    fn dotted_english_titles_are_preserved() {
        let mut context = CompileContext::default();
        extract_profiles(
            "Mr. Bennet greeted Mrs. Bennet, while Mr. Darcy waited.",
            &mut context,
            None,
            &[],
            &crate::language::EnglishLanguageProvider::new(),
        );

        assert!(
            context
                .entities
                .iter()
                .any(|entity| entity.name == "Mr. Bennet"),
            "Dotted honorific normalization must preserve `Mr. Bennet`; found {:?}",
            context
                .entities
                .iter()
                .map(|entity| entity.name.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            context
                .profiles
                .iter()
                .any(|profile| profile.key == "title" && profile.value == "Mr."),
            "A discovered dotted honorific must create the matching title profile"
        );
    }

    /// Objective: Verify Project Gutenberg administrative text is not literary evidence.
    /// Invariants: License headings produce neither entities nor title profiles.
    #[test]
    fn gutenberg_metadata_is_not_a_person() {
        let mut context = CompileContext::default();
        extract_profiles(
            "General Terms of Use and Redistributing Project Gutenberg-tm electronic works\nGeneral Information About Project Gutenberg-tm electronic works",
            &mut context,
            None,
            &[],
            &crate::language::EnglishLanguageProvider::new(),
        );

        assert!(
            context.entities.is_empty(),
            "Gutenberg administrative headings must not create people; found {:?}",
            context
                .entities
                .iter()
                .map(|entity| entity.name.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            context.profiles.is_empty(),
            "Gutenberg administrative headings must not create title profiles"
        );
    }
}
