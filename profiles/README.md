# LoreScope Profiles

JSON profiles that configure how the general compiler distills a text corpus.
Each profile describes one source document: how to split it into segments,
which entities to look for, how to recognize dialog, and how to bias relation
extraction. The four bundled profiles cover the Chinese classical novels
(`sanguo.json`, `shuihu.json`, `honglou.json`, `xiyou.json`) and were
mechanically extracted from the former hardcoded `src/ingest/characters.rs`.

## Files

| File         | Title     | Characters | Factions                          |
|--------------|-----------|-----------|-----------------------------------|
| sanguo.json  | 三国演义  | 51        | 蜀 / 魏 / 吴 / 群雄               |
| shuihu.json  | 水浒传    | 90        | 梁山 / 朝廷                       |
| honglou.json | 红楼梦    | 26        | 贾府 / 薛家 / 王家                |
| xiyou.json   | 西游记    | 18        | 取经 / 佛派 / 天庭                |

## Schema

Every profile is a single JSON object. All keys are required by the loader;
fields the document does not use are set to `null` (for nullable scalars/lists)
or `{}` / `[]` (for objects/arrays). The compiler treats `null` verb/keyword
lists as "use the built-in defaults."

```json
{
  "title": "三国演义",
  "doc_type": "novel",
  "segment_marker": {"kind": "chapter", "start": "第", "end": "回"},
  "entity_hints": [
    {
      "name": "刘备",
      "aliases": ["玄德", "刘皇叔", "刘玄德", "先主", "刘豫州"],
      "single_char": "备",
      "object_type": "person",
      "properties": {}
    }
  ],
  "relation_rules": null,
  "strong_verbs": null,
  "dialog_verbs": null,
  "action_verbs": null,
  "clothing_kw": null,
  "personality_kw": null,
  "death_kw": null,
  "dialog_markers": ["曰：", "道："],
  "min_co_occurrence": 3,
  "factions": {
    "蜀": ["刘备", "关羽", "张飞"]
  },
  "version": "0.1.0"
}
```

### Field reference

| Field             | Type              | Purpose |
|-------------------|-------------------|---------|
| `title`           | string \| null    | Human-readable document title. Surfaced in generated output and logs. `null` when untitled. |
| `doc_type`        | string            | Document category. Currently `"novel"`. Reserved for future types (`"history"`, `"script"`, …). |
| `segment_marker`  | object            | How the raw text is split into units of analysis (chapters, messages, single block). See below. |
| `entity_hints`    | array \| null     | Seed list of known entities (usually characters). **Optional.** When omitted, the compiler falls back to dialog-pattern auto-discovery (`discover_novel_speakers`). |
| `relation_rules`  | array \| null     | Custom relation-detection rules. `null` = use the built-in keyword/verb heuristics. |
| `strong_verbs`    | array \| null     | Verbs that signal a strong directed relation (e.g. `"斩"`, `"拜"`). `null` = compiler defaults. |
| `dialog_verbs`    | array \| null     | Verbs that introduce speech (`"曰"`, `"道"`, `"言"`). `null` = compiler defaults. |
| `action_verbs`    | array \| null     | Verbs that mark an action beat (`"领兵"`, `"挺枪"`). `null` = compiler defaults. |
| `clothing_kw`     | array \| null     | Keywords that trigger the clothing attribute extractor. `null` = compiler defaults. |
| `personality_kw`  | array \| null     | Keywords that trigger the personality attribute extractor. `null` = compiler defaults. |
| `death_kw`        | array \| null     | Keywords that mark a death event (`"身死"`, `"阵亡"`). `null` = compiler defaults. |
| `dialog_markers`  | array of strings  | Substrings that delimit spoken dialog. Almost always `["曰：", "道："]` for classical Chinese. Used by both the auto-discovery pass and the dialog speaker extractor. |
| `min_co_occurrence` | integer         | Minimum number of segments in which two entities must co-occur before a relation edge is emitted. Lower = noisier, higher = sparser graph. The bundled profiles use `3`. |
| `factions`        | object            | Map of faction name → list of member names. Drives the faction-bonus factor during relation scoring (same faction +20%, different faction −20%) and the faction constraint filter. `{}` when the document has no factions. |
| `version`         | string            | Profile schema version. Currently `"0.1.0"`. |

### `entity_hints` entries

Each entry of `entity_hints` has the shape:

```json
{
  "name": "张飞",
  "aliases": ["翼德", "张翼德"],
  "single_char": "飞",
  "object_type": "person",
  "properties": {}
}
```

| Field          | Type             | Purpose |
|----------------|------------------|---------|
| `name`         | string           | Canonical name. All relations and events attach to this string. |
| `aliases`      | array of strings | Alternate names/titles/courtesy names. Each alias resolves back to `name` via the per-document alias map. Use `[]` when there are none. |
| `single_char`  | string \| null   | Single-character shortname used in classical dialog (`"飞曰"`). Matched **only** in safe contexts: preceded by punctuation/string-start and followed by a dialog or action verb. Bare single-char matching is intentionally avoided because it produces catastrophic false positives (`"云"` matches `"云长"`, `"乌云"`, `"飞马"`). `null` when no safe shortname exists. |
| `object_type`  | string           | Entity category. `"person"` for the bundled profiles; reserved for `"place"`, `"organization"`, `"artifact"`, `"concept"`, etc. |
| `properties`   | object           | Free-form attribute bag (faction, title, role, …). Not interpreted by the core compiler; downstream stages may read it. Use `{}` when empty. |

### `segment_marker` kinds

`segment_marker` selects how the input text is broken into segments before any
extraction runs. The `kind` field selects the strategy; the remaining fields
are kind-specific.

| `kind`     | Extra fields      | When to use |
|------------|-------------------|-------------|
| `"chapter"` | `start`, `end`   | Classical novels. A segment runs from a line beginning with `start` (e.g. `"第"`) up to the next occurrence of `end` (e.g. `"回"`). All four bundled profiles use `{"kind":"chapter","start":"第","end":"回"}`. |
| `"single"`  | — (none)         | Unsegmented prose: essays, short stories, single-chapter documents. The whole input is treated as one segment. Use `{"kind":"single"}`. |
| `"message"` | — (none)         | Conversations: chat logs, IM transcripts, transcript-style input. Each message is its own segment; the loader reads the `messages` array from the input JSON rather than splitting raw text. Use `{"kind":"message"}`. |

## Writing a new profile

To distill an arbitrary text, copy the template below, fill in the title and
segment strategy, and either populate `entity_hints` (faster, more precise)
or drop it to rely on auto-discovery.

### Minimal profile (auto-discovery)

For text where you do not know the characters in advance, set `entity_hints`
to `null`. The compiler scans every `dialog_markers` hit, extracts the
speaker name before it, and emits any name that appears in at least
`min_co_occurrence` distinct segments. This catches minor characters that
seeded hints would miss, at the cost of some false positives.

```json
{
  "title": "某部笔记",
  "doc_type": "novel",
  "segment_marker": {"kind": "single"},
  "entity_hints": null,
  "relation_rules": null,
  "strong_verbs": null,
  "dialog_verbs": null,
  "action_verbs": null,
  "clothing_kw": null,
  "personality_kw": null,
  "death_kw": null,
  "dialog_markers": ["曰：", "道："],
  "min_co_occurrence": 3,
  "factions": {},
  "version": "0.1.0"
}
```

### Seeded profile (faster, more precise)

When you know the cast in advance, list them in `entity_hints`. Seeded
entities are always resolved (even if they never speak), and their aliases
disambiguate courtesy-name collisions that auto-discovery cannot.

```json
{
  "title": "聊斋志异",
  "doc_type": "novel",
  "segment_marker": {"kind": "chapter", "start": "第", "end": "卷"},
  "entity_hints": [
    {"name": "蒲松龄", "aliases": ["柳泉居士", "聊斋先生"], "single_char": null, "object_type": "person", "properties": {}}
  ],
  "relation_rules": null,
  "strong_verbs": null,
  "dialog_verbs": null,
  "action_verbs": null,
  "clothing_kw": null,
  "personality_kw": null,
  "death_kw": null,
  "dialog_markers": ["曰：", "道："],
  "min_co_occurrence": 3,
  "factions": {},
  "version": "0.1.0"
}
```

### Conversation profile

For chat transcripts, set `segment_marker` to `{"kind":"message"}` and leave
`entity_hints` as `null` (or seed it with speaker handles). The input file
should carry a `messages` array; each message becomes one segment.

```json
{
  "title": "support-chat-2026-07-28",
  "doc_type": "novel",
  "segment_marker": {"kind": "message"},
  "entity_hints": null,
  "relation_rules": null,
  "strong_verbs": null,
  "dialog_verbs": null,
  "action_verbs": null,
  "clothing_kw": null,
  "personality_kw": null,
  "death_kw": null,
  "dialog_markers": [":"],
  "min_co_occurrence": 2,
  "factions": {},
  "version": "0.1.0"
}
```

## Notes

- All JSON must be valid: no trailing commas, no comments, no unquoted keys.
- Nullable fields use JSON `null`, not the string `"null"`.
- `factions` is always an object; use `{}` (not `null`) when empty so the
  faction-bonus code path can iterate it without a null check.
- `dialog_markers` is required even when `entity_hints` is `null`, because
  auto-discovery depends on it.
- The bundled `factions` data is copied verbatim from
  `config/faction_map.json`; if that file is edited, the corresponding
  profile should be regenerated to match.
