#! /usr/bin/env python3
"""Corpus frequency analysis — extract common verbs for the built-in dictionary.
Usage: python scripts/corpus_freq.py
Output: prints frequency tables for English and Chinese verbs found in the corpora.
"""

import json
import re
import sys
from pathlib import Path
from collections import Counter
from typing import List, Tuple

REPO = Path(__file__).resolve().parents[1]
CORPUS = REPO / "corpus"

# ── English verb heuristics ────────────────────────────────────────────────

# Common English irregular verbs that won't end in -ed/-ing/-s
IRREGULAR_VERBS = {
    "said", "told", "went", "came", "took", "made", "saw", "knew", "thought",
    "brought", "left", "felt", "found", "gave", "got", "heard", "held", "kept",
    "let", "lay", "led", "lost", "meant", "met", "paid", "put", "ran", "read",
    "rose", "sent", "set", "shut", "sat", "spent", "stood", "struck", "took",
    "taught", "won", "wrote", "wept", "slept", "crept", "knelt", "shook",
    "rode", "rose", "threw", "wore", "grew", "drew", "flew", "sang", "swam",
    "began", "bound", "bit", "blew", "broke", "built", "burnt", "caught",
    "chose", "clung", "dug", "drank", "drove", "ate", "fell", "fought",
    "fled", "flung", "forbade", "forgave", "froze", "hid", "hung", "hurt",
    "knelt", "leapt", "lent", "lit", "overcame", "pled", "rang", "sank",
    "slew", "slid", "sought", "spoke", "sprang", "stole", "stung", "stank",
    "strode", "struck", "stung", "swore", "swept", "swung", "tore", "threw",
    "understood", "undertook", "woke", "wound", "wrung",
}

# Words that are NOT verbs despite matching patterns
NON_VERBS = {
    "passed", "presented", "interested", "united", "related", "attached",
    "assembled", "connected", "married", "prepared", "dressed", "educated",
    "complicated", "advanced", "continued", "expressed", "disappeared",
    "preferred", "developed", "happened", "exclaimed", "whispered", "murmured",
    "answered", "repeated", "observed", "remarked", "appeared", "belonged",
    "resembled", "recovered", "apologised", "apologized",
    # -ing words that are more often nouns
    "morning", "evening", "dining", "living", "being", "nothing", "something",
    "everything", "feeling", "meaning", "building", "meeting", "beginning",
    "sitting", "reading", "writing", "painting", "clothing", "blessing",
    "ceiling", "wedding", "heading", "landing", "suffering", "following",
    "surrounding", "understanding", "belongings",
    # -s words that are more often plural nouns
    "mens", "monsieur", "madame", "mademoiselle",
}

def normalize(word: str) -> str:
    w = word.strip("'\".,;:!?()-[]{}«»—–-").lower()
    return w if w else ""

def is_verb(word: str) -> bool:
    w = normalize(word)
    if not w or len(w) < 2:
        return False
    if w in NON_VERBS:
        return False
    if w in IRREGULAR_VERBS:
        return True
    if w.endswith("ed") or w.endswith("ing"):
        return True
    # Third-person singular: only short words ending in 's'
    if w.endswith("s") and len(w) <= 7 and w not in ["this", "thus", "his", "has", "was", "is", "does", "goes"]:
        candidate = w.rstrip("s")
        if len(candidate) >= 2:
            return True
    return False


# ── Chinese verb extraction ────────────────────────────────────────────────

def extract_chinese_verbs_heuristic(texts: List[str]) -> Counter:
    """Heuristic: pick frequent characters that appear in verb-like positions.
    We scan for known Chinese verb characters that commonly appear in literature."""
    
    # Known common literary Chinese verb characters
    COMMON_CORE_VERBS = {"杀", "斩", "擒", "救", "打", "战", "斗", "败", "胜",
                         "攻", "破", "逃", "死", "绑", "缚", "骂", "哭", "笑",
                         "怒", "拜", "封", "赐", "赏", "赦", "围", "刺", "射",
                         "救", "嫁", "娶", "降", "服", "伏", "跪", "立", "坐",
                         "走", "来", "去", "入", "出", "到", "回", "过", "起",
                         "开", "合", "取", "放", "提", "拉", "推", "抱", "举",
                         "言", "曰", "道", "问", "答", "叫", "呼", "喊", "说",
                         "见", "看", "观", "望", "视", "听", "闻", "知", "识",
                         "食", "饮", "喝", "吃", "穿", "戴", "执", "持", "握",
                         "披", "挂", "带", "佩", "乘", "骑", "登", "上", "下",
                         "进", "退", "追", "赶", "逐", "奔", "驰", "行", "停",
                         "居", "住", "处", "在", "有", "无", "得", "失", "成",
                         "败", "兴", "亡", "起", "落", "变", "化", "改", "易",
                         "抚", "按", "指", "示", "告", "传", "报", "咨", "呈",
                         "乞", "请", "求", "告", "劝", "谏", "止", "许", "允",
                         "谢", "辞", "让", "受", "纳", "献", "奉", "贡", "进",
                         "通", "达", "具", "备", "整", "理", "治", "安", "定",
                         "统", "领", "率", "带", "部", "分", "布", "列", "摆",
                         "装", "扮", "作", "为", "当", "称", "号", "名", "谓",
                         "似", "如", "若", "类", "由", "从", "随", "跟", "陪",
                         "待", "候", "等", "料", "想", "念", "思", "虑", "忧",
                         "愁", "悲", "喜", "惊", "恐", "惧", "怕", "怜", "爱",
                         "憎", "恨", "怨", "怒", "恼", "烦", "闷", "慌", "忙",
                         "急", "促", "厉", "严", "肃", "恭", "敬", "尊", "重",
    }
    counter = Counter()
    for text in texts:
        for ch in text:
            if ch in COMMON_CORE_VERBS:
                counter[ch] += 1
    return counter


# ── Main ────────────────────────────────────────────────────────────────────

def analyze_english(file: str) -> Tuple[Counter, int]:
    path = CORPUS / file
    text = path.read_text(encoding="utf-8")
    total = len(text)
    # Split into words
    words = re.findall(r"[A-Za-z]+", text)
    verb_counter = Counter()
    for w in words:
        if is_verb(w):
            verb_counter[w.lower()] += 1
    return verb_counter, total


def analyze_chinese(file: str) -> Counter:
    path = CORPUS / file
    raw = path.read_bytes()
    # Try common Chinese encodings
    for enc in ("utf-8", "gb18030", "gbk", "gb2312", "latin-1"):
        try:
            text = raw.decode(enc)
            break
        except (UnicodeDecodeError, LookupError):
            continue
    else:
        text = raw.decode("utf-8", errors="replace")
    return extract_chinese_verbs_heuristic([text])


def print_english_table(name: str, counter: Counter, top_n: int = 40) -> None:
    total = sum(counter.values())
    print(f"\n{'='*70}")
    print(f"  English verbs from «{name}»  ({total:,} total occurrences)")
    print(f"{'='*70}")
    print(f"  {'#':>4}  {'Verb':<22}  {'Count':>8}  {'%':>6}")
    print(f"  {'-'*4}  {'-'*22}  {'-'*8}  {'-'*6}")
    for i, (word, count) in enumerate(counter.most_common(top_n), 1):
        pct = count / total * 100
        print(f"  {i:>4}  {word:<22}  {count:>8,}  {pct:>5.1f}%")


def print_chinese_table(name: str, counter: Counter, top_n: int = 50) -> None:
    total = sum(counter.values())
    print(f"\n{'='*70}")
    print(f"  Chinese verb chars from «{name}»  ({total:,} total occurrences)")
    print(f"{'='*70}")
    print(f"  {'#':>4}  {'Char':<6}  {'Count':>8}  {'%':>6}")
    print(f"  {'-'*4}  {'-'*6}  {'-'*8}  {'-'*6}")
    for i, (ch, count) in enumerate(counter.most_common(top_n), 1):
        pct = count / total * 100
        print(f"  {i:>4}  {ch:<6}  {count:>8,}  {pct:>5.1f}%")


def main():
    print("=" * 70)
    print("  CORPUS FREQUENCY ANALYSIS — Verb extraction for built-in dictionary")
    print("=" * 70)

    # Auto-discover all .txt files in the corpus directory
    txt_files = sorted(CORPUS.glob("*.txt"))

    english_files = []
    chinese_files = []
    for f in txt_files:
        # Detect encoding: try utf-8, fall back to gb18030 for Chinese legacy encodings
        raw = f.read_bytes()[:5000]
        try:
            text_sample = raw.decode("utf-8")
        except UnicodeDecodeError:
            try:
                text_sample = raw.decode("gb18030")
            except UnicodeDecodeError:
                text_sample = raw.decode("latin-1", errors="replace")
        ascii_chars = sum(1 for c in text_sample if c.isascii())
        if ascii_chars > len(text_sample) * 0.8:
            english_files.append(f.name)
        else:
            chinese_files.append(f.name)

    # ── English ──────────────────────────────────────────────────────────
    for fname in english_files:
        vc, total = analyze_english(fname)
        print_english_table(fname.replace(".txt", ""), vc)

    # ── Chinese ──────────────────────────────────────────────────────────
    for fname in chinese_files:
        vc = analyze_chinese(fname)
        print_chinese_table(fname.replace(".txt", ""), vc)

    # ── Combined English ────────────────────────────────────────────────
    combined_en = Counter()
    for fname in english_files:
        vc, _ = analyze_english(fname)
        combined_en += vc
    print_english_table("TOTAL English", combined_en, top_n=50)

    # ── Combined Chinese ────────────────────────────────────────────────
    combined_zh = Counter()
    for fname in chinese_files:
        vc = analyze_chinese(fname)
        combined_zh += vc
    print_chinese_table("TOTAL Chinese", combined_zh, top_n=70)

    # ── P6: unknown high-frequency candidate report ────────────────────
    print_candidate_report(combined_en, combined_zh)

    # ── Output minimal Rust source for dictionary.rs ────────────────────
    print("\n\n")
    print("=" * 70)
    print("  GENERATED RUST CODE — paste into src/dictionary.rs")
    print("=" * 70)

    # English strong verbs (from top combined, filtered manually)
    en_strong = [w for w, _ in combined_en.most_common(80) if len(w) >= 4]
    print(f"""
/// English strong verbs — extracted from War and Peace + Pride and Prejudice.
pub fn english_strong_verbs() -> &'static [&'static str] {{
    &[
        {', '.join(f'"{w}"' for w in en_strong[:40])}
    ]
}}

/// English action/emotion verbs — less violent, more dialog/motion oriented.
pub fn english_action_verbs() -> &'static [&'static str] {{
    &[
        {', '.join(f'"{w}"' for w in en_strong[40:80])}
    ]
}}
""")

    # Chinese verb chars
    zh_top = [ch for ch, _ in combined_zh.most_common(60) if ch]
    # Split into two groups
    mid = len(zh_top) // 2
    print(f"""
/// Chinese strong verbs — from 三国演义 + 封神演义.
pub fn chinese_strong_verbs() -> &'static [&'static str] {{
    &[
        {', '.join(f'"{w}"' for w in zh_top[:mid])}
    ]
}}

/// Chinese action verbs.
pub fn chinese_action_verbs() -> &'static [&'static str] {{
    &[
        {', '.join(f'"{w}"' for w in zh_top[mid:])}
    ]
}}
""")

    # ── Elite Score ────────────────────────────────────────────────────
    print("\n\n")
    print("=" * 70)
    print("  ELITE SCORE — candidate verb ranking")
    print("=" * 70)
    print("""
elite_score =
    0.25 × semantic_impact   (number of cognitive effects / fact types)
  + 0.20 × cross_domain      (appears in ≥2 texts = 1.0, 1 text = 0.5)
  + 0.20 × corpus_frequency  (normalized log-frequency rank)
  + 0.15 × precision         (verb length / specificity heuristic)
  + 0.10 × test_coverage     (has existing test coverage — currently 0 for all)
  + 0.10 × maintenance       (core vocabulary stability, default 0.8)
""")

    n_files = {"en": len(english_files), "zh": len(chinese_files)}

    for lang, combined, top_n in [
        ("en", combined_en, 40),
        ("zh", combined_zh, 60),
    ]:
        total_count = sum(combined.values())
        max_count = combined.most_common(1)[0][1] if combined else 1
        domain_count = n_files[lang]

        print(f"\n  ── Top {top_n} {lang.upper()} verbs by elite_score ──\n")
        print(f"  {'Rank':>4}  {'Verb':<24}  {'Score':>6}  {'Impact':>6}  {'Domain':>6}  {'Freq':>6}")
        print(f"  {'----':>4}  {'----':<24}  {'-----':>6}  {'------':>6}  {'------':>6}  {'----':>6}")

        scored = []
        for i, (word, count) in enumerate(combined.most_common(top_n)):
            freq = count / max_count
            freq_norm = min(freq, 1.0)

            # semantic_impact: heuristic based on word type
            is_attack = any(w in word.lower() for w in ["kill", "attack", "strike", "murder", "kill"])
            is_emotion = any(w in word.lower() for w in ["cry", "laugh", "smile", "frown", "weep", "fear"])
            is_speech = any(w in word.lower() for w in ["say", "ask", "reply", "shout", "whisper"])
            is_movement = any(w in word.lower() for w in ["go", "come", "enter", "leave", "ride", "walk"])
            if lang == "zh":
                semantic_impact = 0.7  # Chinese single chars generally have multiple meanings
            elif is_attack or is_emotion or is_speech or is_movement:
                semantic_impact = 0.9
            elif len(word) >= 5:
                semantic_impact = 0.8
            elif len(word) >= 3:
                semantic_impact = 0.6
            else:
                semantic_impact = 0.4

            # cross_domain_value: how many texts the word appears in
            # (approximation using the word's total count distribution)
            if count > total_count * 0.01:
                cross_domain = 0.9
            elif count > total_count * 0.001:
                cross_domain = 0.7
            else:
                cross_domain = 0.4

            # precision: longer words are less ambiguous
            if lang == "zh":
                precision = 0.5  # single Chinese chars have high ambiguity
            else:
                precision = min(len(word) / 8, 1.0) * 0.8 + 0.2

            # test_coverage: 0 for all candidates
            test_cov = 0.0
            maintenance = 0.8

            elite = (
                0.25 * semantic_impact
                + 0.20 * cross_domain
                + 0.20 * freq_norm
                + 0.15 * precision
                + 0.10 * test_cov
                + 0.10 * maintenance
            )
            scored.append((elite, word, semantic_impact, cross_domain, freq_norm))

        scored.sort(reverse=True)
        for rank, (score, word, impact, domain, freq) in enumerate(scored[:top_n], 1):
            print(f"  {rank:>4}  {word:<24}  {score:.3f}  {impact:.3f}  {domain:.3f}  {freq:.3f}")


# ── P6: unknown high-frequency candidate report ─────────────────────────────

def load_lexicon_forms() -> set:
    """Load lemma + forms from config/dictionary.json and all domain packs.

    Mirrors the Rust loader (`load_lexemes_from_file`): both a bare JSON
    array and an object with a `lexemes` key are accepted.
    """
    forms = set()
    paths = [REPO / "config" / "dictionary.json"]
    packs_dir = REPO / "lexicon" / "packs"
    if packs_dir.is_dir():
        paths.extend(sorted(packs_dir.glob("*.json")))
    for path in paths:
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        lexemes = data if isinstance(data, list) else data.get("lexemes", [])
        for lex in lexemes:
            if not isinstance(lex, dict):
                continue
            lemma = lex.get("lemma", "")
            if isinstance(lemma, str):
                forms.add(lemma.lower())
            for f in lex.get("forms", []):
                if isinstance(f, str):
                    forms.add(f.lower())
    return forms


def print_candidate_report(combined_en: Counter, combined_zh: Counter) -> None:
    """Report high-frequency corpus words NOT in the lexicon (P6 candidates)."""
    print("\n\n")
    print("=" * 70)
    print("  P6 CANDIDATE REPORT — high-frequency words missing from lexicon")
    print("=" * 70)
    print("""
Candidates are produced by automatic statistics ONLY. Per §10.1, they must
not auto-enter Core: promote via Candidate → Experimental → Core after
adding semantics, constraints, and positive/negative tests.
""")

    known = load_lexicon_forms()

    for lang, counter, top_n in [("EN", combined_en, 30), ("ZH", combined_zh, 40)]:
        print(f"  ── {lang} unknown high-frequency candidates ──\n")
        print(f"  {'#':>4}  {'Word':<26}  {'Count':>8}")
        shown = 0
        for word, count in counter.most_common():
            w = word.lower()
            if w in known:
                continue
            # Skip stop-word-like noise for English candidates.
            if lang == "EN" and w in {
                "said", "mrs", "miss", "its", "yes", "always", "having",
                "words", "anything", "others", "thing", "things", "hands",
                "troops", "horses", "during", "towards", "indeed", "perhaps",
                "less", "days", "news", "orders", "means", "nothing", "evening",
            }:
                continue
            print(f"  {shown + 1:>4}  {word:<26}  {count:>8,}")
            shown += 1
            if shown >= top_n:
                break
        if shown == 0:
            print("  (all high-frequency words are already in the lexicon)")
        print()


if __name__ == "__main__":
    main()
