#!/usr/bin/env python3
"""AnIME 大规模真实英文输入评测基准构建器（支持开源真实语料防泄漏切分及可选本机非商用评测适配器）。"""

from __future__ import annotations

import argparse
import gzip
import json
import random
import re
import sys
from collections import defaultdict
from pathlib import Path
from typing import Iterable, TextIO

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from build_misspellings import (  # noqa: E402
    is_clean_token,
    parse_typos_csv,
    parse_wiki_for_machines,
)
from common import in_split  # noqa: E402

CATEGORIES = (
    "exact",
    "prefix",
    "typo_simple",
    "typo_prefix",
    "typo_phonetic",
    "context_realword",
    "sentence",
)

CLEAN_EVAL_INPUT_RE = re.compile(r"^[A-Za-z][A-Za-z' -]*[A-Za-z]$|^[A-Za-z]$")
ALPHA_WORD_RE = re.compile(r"^[A-Za-z]+(?:'[A-Za-z]+)?$")
CAMEL_OR_ALLCAPS_RE = re.compile(r"([a-z][A-Z]|[A-Z]{3,})")

QWERTY_NEIGHBORS: dict[str, str] = {
    "a": "qwsz",
    "b": "vghn",
    "c": "xdfv",
    "d": "erfcxs",
    "e": "wrsd",
    "f": "rtgvcd",
    "g": "tyhbvf",
    "h": "yujnbg",
    "i": "uokj",
    "j": "uikmnh",
    "k": "iolmj",
    "l": "opk",
    "m": "njk",
    "n": "bhjm",
    "o": "iplk",
    "p": "ol",
    "q": "wa",
    "r": "etdf",
    "s": "wedxza",
    "t": "ryfg",
    "u": "yihj",
    "v": "cfgb",
    "w": "qesa",
    "x": "zsdc",
    "y": "tugh",
    "z": "asx",
}

PHONETIC_CLUSTERS = (
    ("ph", "f"),
    ("gh", "f"),
    ("kn", "n"),
    ("pn", "n"),
    ("ps", "s"),
    ("wr", "r"),
    ("sc", "s"),
    ("eau", "o"),
    ("sch", "sk"),
    ("sch", "sh"),
    ("ch", "k"),
    ("ough", "u"),
    ("ough", "o"),
    ("tion", "shun"),
    ("qu", "kw"),
    ("dg", "g"),
)

# 英文真实语境易混真词对（用于从 MultiWOZ 等真实对话抽取 context_realword）
REALWORD_CONFUSIONS: dict[str, tuple[str, ...]] = {
    "from": ("form",),
    "form": ("from",),
    "their": ("there",),
    "there": ("their",),
    "your": ("you're", "yur"),
    "you're": ("your",),
    "its": ("it's",),
    "it's": ("its",),
    "to": ("too",),
    "too": ("to",),
    "than": ("then",),
    "then": ("than",),
    "affect": ("effect",),
    "effect": ("affect",),
    "accept": ("except",),
    "except": ("accept",),
    "lose": ("loose",),
    "loose": ("lose",),
    "weather": ("whether",),
    "whether": ("weather",),
    "quiet": ("quite",),
    "quite": ("quiet",),
    "hear": ("here",),
    "here": ("hear",),
    "know": ("no",),
    "no": ("know",),
    "write": ("right",),
    "right": ("write",),
    "where": ("were", "wear"),
    "were": ("where",),
    "wear": ("where",),
    "of": ("off",),
    "off": ("of",),
    "an": ("and",),
    "and": ("an",),
}

APOSTROPHE_DROPS: dict[str, str] = {
    "i'm": "im",
    "i've": "ive",
    "i'll": "ill",
    "i'd": "id",
    "don't": "dont",
    "can't": "cant",
    "won't": "wont",
    "isn't": "isnt",
    "aren't": "arent",
    "didn't": "didnt",
    "doesn't": "doesnt",
    "wasn't": "wasnt",
    "weren't": "werent",
    "haven't": "havent",
    "hasn't": "hasnt",
    "hadn't": "hadn't".replace("'", ""),
    "wouldn't": "wouldnt",
    "couldn't": "couldnt",
    "shouldn't": "shouldnt",
    "they're": "theyre",
    "you're": "youre",
    "we're": "were",
    "it's": "its",
    "that's": "thats",
    "there's": "theres",
    "what's": "whats",
    "who's": "whos",
    "let's": "lets",
}


def damerau_levenshtein(a: str, b: str) -> int:
    """计算最优字符串对齐（含相邻字符换位作为 1 次编辑）的编辑距离。"""
    la, lb = len(a), len(b)
    if a == b:
        return 0
    if la == 0:
        return lb
    if lb == 0:
        return la
    dp = [[0] * (lb + 1) for _ in range(la + 1)]
    for i in range(la + 1):
        dp[i][0] = i
    for j in range(lb + 1):
        dp[0][j] = j
    for i in range(1, la + 1):
        for j in range(1, lb + 1):
            cost = 0 if a[i - 1] == b[j - 1] else 1
            best = min(dp[i - 1][j] + 1, dp[i][j - 1] + 1, dp[i - 1][j - 1] + cost)
            if i > 1 and j > 1 and a[i - 1] == b[j - 2] and a[i - 2] == b[j - 1]:
                best = min(best, dp[i - 2][j - 2] + 1)
            dp[i][j] = best
    return dp[la][lb]


def is_phonetic_misspelling(typo_l: str, target_l: str) -> bool:
    if damerau_levenshtein(typo_l, target_l) >= 2:
        return True
    for c_tgt, c_typ in PHONETIC_CLUSTERS:
        if c_tgt in target_l and c_typ in typo_l and c_tgt not in typo_l:
            return True
    return False


def is_valid_sample(cat: str, ctx: str, inp: str, exp: str) -> bool:
    if cat not in CATEGORIES:
        return False
    if "\t" in ctx or "\n" in ctx or "\r" in ctx:
        return False
    if not inp or not exp or "\t" in inp or "\t" in exp or "\n" in inp or "\n" in exp:
        return False
    if "  " in inp or "  " in exp or inp != inp.strip() or exp != exp.strip():
        return False
    if not CLEAN_EVAL_INPUT_RE.match(inp):
        return False
    if CAMEL_OR_ALLCAPS_RE.search(inp) or CAMEL_OR_ALLCAPS_RE.search(exp):
        return False

    il, el = inp.lower(), exp.lower()
    if cat == "exact":
        return inp == exp and " " not in inp
    if cat == "prefix":
        return " " not in inp and el.startswith(il) and len(il) < len(el)
    if cat == "typo_prefix":
        return " " not in inp and (not el.startswith(il)) and len(il) < len(el)
    if cat in ("typo_simple", "typo_phonetic"):
        return " " not in inp and il != el
    if cat == "sentence":
        return (" " in inp) and (" " in exp) and CLEAN_EVAL_INPUT_RE.match(exp) is not None
    return True


def open_text_maybe_gz(path: Path) -> TextIO:
    if path.suffix == ".gz":
        return gzip.open(path, "rt", encoding="utf-8", errors="replace")
    return path.open("r", encoding="utf-8", errors="replace")


def load_lexicon_tsv(path: Path) -> tuple[list[tuple[str, float, int]], dict[str, tuple[str, float]]]:
    entries: list[tuple[str, float, int]] = []
    by_lower: dict[str, tuple[str, float]] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        s = raw.strip()
        if not s or s.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) < 3:
            continue
        word = parts[0].strip()
        if not is_clean_token(word):
            continue
        zipf = float(parts[1].strip())
        rank = int(parts[2].strip())
        entries.append((word, zipf, rank))
        low = word.lower()
        if low not in by_lower or zipf > by_lower[low][1]:
            by_lower[low] = (word, zipf)
    return entries, by_lower


def extract_lexicon_samples(
    entries: list[tuple[str, float, int]],
    rng: random.Random,
    exact_limit: int,
    prefix_limit: int,
    typo_prefix_limit: int,
) -> dict[str, list[tuple[str, str, str, str, str]]]:
    out: dict[str, list[tuple[str, str, str, str, str]]] = {
        "exact": [],
        "prefix": [],
        "typo_prefix": [],
    }
    if not entries:
        return out

    # 1. 分层抽样 exact：高频 / 中频 / 长尾 / 大写专名 / 撇号缩写
    high = [w for w, z, _ in entries if z >= 5.0 and not CAMEL_OR_ALLCAPS_RE.search(w) and len(w) >= 2]
    mid = [w for w, z, _ in entries if 3.5 <= z < 5.0 and not CAMEL_OR_ALLCAPS_RE.search(w) and len(w) >= 3]
    tail = [w for w, z, _ in entries if 2.4 <= z < 3.5 and not CAMEL_OR_ALLCAPS_RE.search(w) and len(w) >= 4]
    caps_or_contr = [
        w
        for w, z, _ in entries
        if z >= 3.2 and (any(c.isupper() for c in w) or "'" in w) and not CAMEL_OR_ALLCAPS_RE.search(w)
    ]

    for bucket in (high, mid, tail, caps_or_contr):
        rng.shuffle(bucket)

    quotas = [
        (high, max(1, int(exact_limit * 0.35)), "high-freq lexicon word"),
        (mid, max(1, int(exact_limit * 0.30)), "mid-freq lexicon word"),
        (tail, max(1, int(exact_limit * 0.20)), "tail-freq lexicon word"),
        (caps_or_contr, exact_limit, "proper/contraction lexicon word"),
    ]
    for bucket, quota, note in quotas:
        for w in bucket[:quota]:
            if len(out["exact"]) >= exact_limit:
                break
            out["exact"].append(("exact", "", w, w, f"lexicon-stratified: {note}"))

    # 2. 构建无歧义前缀索引（Top 30k 高频小写英文词），要求第 1 名与第 2 名存在明显 Zipf 优势或唯一
    clean_alpha = [
        (w, z)
        for w, z, _ in entries[:40000]
        if w.islower() and w.isalpha() and len(w) >= 6 and z >= 2.8
    ]
    prefix_top2: dict[str, list[tuple[str, float]]] = defaultdict(list)
    for w, z in clean_alpha:
        max_p = min(len(w) - 2, 10)
        for plen in range(4, max_p + 1):
            p = w[:plen]
            lst = prefix_top2[p]
            if len(lst) < 2:
                lst.append((w, z))

    prefix_cands: list[tuple[str, str, str]] = []
    for p, lst in prefix_top2.items():
        top_w, top_z = lst[0]
        margin = top_z - lst[1][1] if len(lst) >= 2 else 2.0
        if margin >= 0.8:
            prefix_cands.append((p, top_w, f"lexicon-unambiguous-prefix (margin={margin:.2f})"))
    rng.shuffle(prefix_cands)

    used_prefix_targets: set[str] = set()
    for p, top_w, note in prefix_cands:
        if len(out["prefix"]) >= prefix_limit:
            break
        if top_w in used_prefix_targets:
            continue
        used_prefix_targets.add(top_w)
        out["prefix"].append(("prefix", "", p, top_w, note))

    # 3. 合成截断错拼前缀（前部 1 处邻键/换位/漏打 + 截断词尾）
    long_alpha = [
        (w, z)
        for w, z, _ in entries[:20000]
        if w.islower() and w.isalpha() and len(w) >= 8 and z >= 3.2
    ]
    rng.shuffle(long_alpha)
    for w, _ in long_alpha:
        if len(out["typo_prefix"]) >= typo_prefix_limit:
            break
        trunc_len = rng.randint(6, min(len(w) - 2, 10))
        stem = w[:trunc_len]
        mode = rng.choice(("transpose", "omit", "neighbor"))
        corrupted = ""
        note = ""
        if mode == "transpose" and len(stem) >= 5:
            pos = rng.randint(1, min(3, len(stem) - 2))
            if stem[pos] != stem[pos + 1]:
                corrupted = stem[:pos] + stem[pos + 1] + stem[pos] + stem[pos + 2 :]
                note = "early-transposition + truncated prefix"
        elif mode == "omit" and len(stem) >= 6:
            pos = rng.randint(1, min(4, len(stem) - 2))
            corrupted = stem[:pos] + stem[pos + 1 :]
            note = "early-omission + truncated prefix"
        else:
            pos = rng.randint(1, min(4, len(stem) - 2))
            nbrs = QWERTY_NEIGHBORS.get(stem[pos], "")
            if nbrs:
                rep = rng.choice(nbrs)
                corrupted = stem[:pos] + rep + stem[pos + 1 :]
                note = "early-qwerty-sub + truncated prefix"
        if corrupted and is_valid_sample("typo_prefix", "", corrupted, w):
            out["typo_prefix"].append(("typo_prefix", "", corrupted, w, f"lexicon-synth: {note}"))

    return out


def extract_typos_from_wordlists(
    wiki_paths: list[Path],
    typos_csv_paths: list[Path],
    by_lower: dict[str, tuple[str, float]],
    split: str,
    eval_pct: int,
    min_target_zipf: float = 3.0,
) -> tuple[list[tuple[str, str, str, str, str]], dict[str, str]]:
    """提取 held-out 无歧义单目标错拼测试样例，并返回反向错拼词典（target_lower -> typo）。"""
    raw: list[tuple[str, str, str, float]] = []
    for p in wiki_paths:
        raw.extend(parse_wiki_for_machines(p))
    for p in typos_csv_paths:
        raw.extend(parse_typos_csv(p))

    grouped: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for typo, target, src, _ in raw:
        grouped[typo].append((target, src))

    rows: list[tuple[str, str, str, str, str]] = []
    reverse_typo: dict[str, str] = {}
    for typo, pairs in grouped.items():
        if not in_split(typo, split, eval_pct):
            continue
        distinct_targets = {t.lower() for t, _ in pairs}
        if len(distinct_targets) != 1:
            # 跳过一词多纠目标（如 aache -> ache, cache），确保基准评测真值无歧义
            continue
        target, src = pairs[0]
        if len(typo) < 3 or len(target) < 3:
            continue
        if by_lower:
            hit = by_lower.get(target.lower())
            if hit is None or hit[1] < min_target_zipf:
                continue
            target_canon = hit[0]
        else:
            target_canon = target

        cat = "typo_phonetic" if is_phonetic_misspelling(typo.lower(), target_canon.lower()) else "typo_simple"
        if is_valid_sample(cat, "", typo, target_canon):
            rows.append((cat, "", typo, target_canon, f"open-wordlist:{src}"))
            reverse_typo.setdefault(target_canon.lower(), typo)
    return rows, reverse_typo


def _clean_phrase_tokens(text: str) -> list[str]:
    stripped = text.strip().rstrip(".!?")
    if not stripped:
        return []
    raw_tokens = stripped.split()
    if any(not ALPHA_WORD_RE.match(tok) for tok in raw_tokens):
        return []
    return raw_tokens


def extract_from_github_typo_corpus(
    jsonl_paths: list[Path],
    by_lower: dict[str, tuple[str, float]],
    split: str,
    eval_pct: int,
    max_per_cat: int,
) -> dict[str, list[tuple[str, str, str, str, str]]]:
    out: dict[str, list[tuple[str, str, str, str, str]]] = {
        "typo_simple": [],
        "typo_phonetic": [],
        "context_realword": [],
        "sentence": [],
    }
    if not jsonl_paths:
        return out

    cap_each = max(max_per_cat * 2, 1000)
    for path in jsonl_paths:
        with open_text_maybe_gz(path) as fh:
            for raw in fh:
                if all(len(v) >= cap_each for v in out.values()):
                    break
                s = raw.strip()
                if not s:
                    continue
                obj = json.loads(s)
                commit = str(obj.get("commit", ""))
                if not in_split(commit or s[:32], split, eval_pct):
                    continue
                for edit in obj.get("edits", []):
                    if not edit.get("is_typo"):
                        continue
                    if float(edit.get("prob_typo", 0.0)) < 0.85:
                        continue
                    src = edit.get("src") or {}
                    tgt = edit.get("tgt") or {}
                    if src.get("lang") != "eng" or tgt.get("lang") != "eng":
                        continue
                    src_txt = str(src.get("text", "")).strip()
                    tgt_txt = str(tgt.get("text", "")).strip()
                    if not src_txt or not tgt_txt or src_txt == tgt_txt:
                        continue
                    # 跳过代码符号、URL、数字或驼峰/全大写标识符
                    if any(ch in src_txt for ch in "{}[]()<>_=/*#@`0123456789\""):
                        continue
                    if any(ch in tgt_txt for ch in "{}[]()<>_=/*#@`0123456789\""):
                        continue
                    if "http" in src_txt.lower() or "http" in tgt_txt.lower():
                        continue

                    src_toks = _clean_phrase_tokens(src_txt)
                    tgt_toks = _clean_phrase_tokens(tgt_txt)
                    if not src_toks or len(src_toks) != len(tgt_toks):
                        continue

                    diffs = [
                        i
                        for i, (sw, tw) in enumerate(zip(src_toks, tgt_toks))
                        if sw != tw
                    ]
                    if not diffs:
                        continue

                    # 检查目标词是否均在英文词库内（过滤专有项目缩写/变量名）
                    if by_lower and not all(tgt_toks[i].lower() in by_lower for i in diffs):
                        continue

                    if len(diffs) == 1:
                        idx = diffs[0]
                        sw, tw = src_toks[idx], tgt_toks[idx]
                        if (
                            2 <= len(sw) <= 24
                            and 2 <= len(tw) <= 24
                            and not CAMEL_OR_ALLCAPS_RE.search(sw)
                            and not CAMEL_OR_ALLCAPS_RE.search(tw)
                        ):
                            sw_is_word = (sw.lower() in by_lower) if by_lower else False
                            if sw_is_word and sw.lower() != tw.lower() and idx >= 1 and len(out["context_realword"]) < cap_each:
                                ctx_toks = src_toks[max(0, idx - 5) : idx]
                                ctx = " ".join(ctx_toks) + " "
                                if idx + 1 < len(src_toks):
                                    inp = f"{sw} {src_toks[idx + 1]}"
                                    exp = f"{tw} {tgt_toks[idx + 1]}"
                                else:
                                    inp, exp = sw, tw
                                if is_valid_sample("context_realword", ctx, inp, exp):
                                    out["context_realword"].append(
                                        ("context_realword", ctx, inp, exp, "github-typo-corpus: real-word edit")
                                    )
                            elif sw.lower() != tw.lower():
                                dl = damerau_levenshtein(sw.lower(), tw.lower())
                                if dl <= 4:
                                    cat = "typo_phonetic" if is_phonetic_misspelling(sw.lower(), tw.lower()) else "typo_simple"
                                    if len(out[cat]) < cap_each and is_valid_sample(cat, "", sw.lower(), tw):
                                        out[cat].append((cat, "", sw.lower(), tw, "github-typo-corpus"))

                    if 4 <= len(src_toks) <= 10 and 1 <= len(diffs) <= 3 and len(out["sentence"]) < cap_each:
                        if by_lower and not all(w.lower() in by_lower for w in tgt_toks):
                            continue
                        inp_sent = " ".join(src_toks)
                        exp_sent = " ".join(tgt_toks)
                        if inp_sent != exp_sent and is_valid_sample("sentence", "", inp_sent, exp_sent):
                            out["sentence"].append(("sentence", "", inp_sent, exp_sent, "github-typo-corpus: real commit"))
    return out


def _corrupt_word_qwerty(word: str, rng: random.Random) -> str | None:
    low = word.lower()
    if low in APOSTROPHE_DROPS:
        return APOSTROPHE_DROPS[low]
    if low == "i":
        return "i"
    if len(low) < 4 or not low.isalpha():
        return None
    op = rng.choice(("transpose", "omit", "sub"))
    if op == "transpose":
        pos = rng.randint(1, len(low) - 2)
        if low[pos] != low[pos + 1]:
            return low[:pos] + low[pos + 1] + low[pos] + low[pos + 2 :]
    if op == "omit":
        pos = rng.randint(1, len(low) - 2)
        return low[:pos] + low[pos + 1 :]
    pos = rng.randint(1, len(low) - 2)
    nbrs = QWERTY_NEIGHBORS.get(low[pos], "")
    if nbrs:
        return low[:pos] + rng.choice(nbrs) + low[pos + 1 :]
    return None


def extract_from_multiwoz(
    multiwoz_files: list[Path],
    by_lower: dict[str, tuple[str, float]],
    reverse_typo: dict[str, str],
    rng: random.Random,
    max_per_cat: int,
    min_target_zipf: float = 3.0,
) -> dict[str, list[tuple[str, str, str, str, str]]]:
    out: dict[str, list[tuple[str, str, str, str, str]]] = {
        "context_realword": [],
        "sentence": [],
    }
    if not multiwoz_files:
        return out

    clauses: list[list[str]] = []
    seen_clauses: set[str] = set()
    for path in sorted(multiwoz_files):
        data = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(data, list):
            continue
        for dlg in data:
            for turn in dlg.get("turns", []):
                if turn.get("speaker") != "USER":
                    continue
                utt = str(turn.get("utterance", "")).strip()
                if not utt:
                    continue
                segments = [utt] + [seg.strip() for seg in re.split(r"[.!?,;]+", utt)]
                for seg in segments:
                    toks = _clean_phrase_tokens(seg)
                    if not (4 <= len(toks) <= 10):
                        continue
                    if any(CAMEL_OR_ALLCAPS_RE.search(t) for t in toks):
                        continue
                    if by_lower:
                        ok = True
                        norm_toks: list[str] = []
                        for i, t in enumerate(toks):
                            hit = by_lower.get(t.lower())
                            if hit is None or hit[1] < min_target_zipf:
                                ok = False
                                break
                            canon = hit[0]
                            if i == 0 and t[0].isupper() and not canon[0].isupper():
                                norm_toks.append(t)
                            elif t.islower() and canon[0].isupper():
                                norm_toks.append(canon)
                            else:
                                norm_toks.append(t)
                        if not ok:
                            continue
                        toks = norm_toks
                    key = " ".join(toks)
                    if key not in seen_clauses:
                        seen_clauses.add(key)
                        clauses.append(toks)

    rng.shuffle(clauses)
    cap_ctx = max(max_per_cat * 2, 1000)
    cap_sent = max(max_per_cat * 2, 1000)

    for toks in clauses:
        if len(out["context_realword"]) >= cap_ctx and len(out["sentence"]) >= cap_sent:
            break

        # 1. 真词混淆 context_realword
        if len(out["context_realword"]) < cap_ctx:
            for idx in range(2, len(toks)):
                exp_w = toks[idx]
                cands = REALWORD_CONFUSIONS.get(exp_w.lower())
                if not cands:
                    continue
                wrong_w = cands[0]
                if exp_w[0].isupper():
                    wrong_w = wrong_w.capitalize()
                ctx = " ".join(toks[max(0, idx - 5) : idx]) + " "
                if idx + 1 < len(toks):
                    inp = f"{wrong_w} {toks[idx + 1]}"
                    exp = f"{exp_w} {toks[idx + 1]}"
                else:
                    inp, exp = wrong_w, exp_w
                if is_valid_sample("context_realword", ctx, inp, exp):
                    out["context_realword"].append(
                        ("context_realword", ctx, inp, exp, "multiwoz-2.2: natural real-word confusion")
                    )
                    break

        # 2. 对话整句 sentence（保留真实英文对话文本 + 注入真实打字噪声）
        if len(out["sentence"]) < cap_sent and 4 <= len(toks) <= 9:
            noisy = list(toks)
            changed = 0
            # 第一步：将 I / 缩写转换为用户高频无撇号/小写打法
            for i, w in enumerate(noisy):
                lw = w.lower()
                if w == "I":
                    noisy[i] = "i"
                    changed += 1
                elif lw in APOSTROPHE_DROPS:
                    noisy[i] = APOSTROPHE_DROPS[lw]
                    changed += 1

            # 第二步：随机挑 1-2 个词注入真实错拼或 QWERTY 噪声
            indices = [i for i, w in enumerate(noisy) if len(w) >= 4 and w.isalpha()]
            rng.shuffle(indices)
            target_typos = rng.randint(1, min(2, max(1, len(indices))))
            added_typos = 0
            for idx in indices:
                if added_typos >= target_typos:
                    break
                lw = noisy[idx].lower()
                rep = reverse_typo.get(lw) or _corrupt_word_qwerty(noisy[idx], rng)
                if rep and rep != lw and is_clean_token(rep) and " " not in rep:
                    noisy[idx] = rep
                    changed += 1
                    added_typos += 1

            if changed > 0:
                # 整句如果原对话首词普通小写或开头非 I，规范期望文本维持原词（除第一词首字母已在对话中）
                inp_sent = " ".join(noisy)
                exp_sent = " ".join(toks)
                if inp_sent != exp_sent and is_valid_sample("sentence", "", inp_sent, exp_sent):
                    out["sentence"].append(
                        ("sentence", "", inp_sent, exp_sent, "multiwoz-2.2: conversational sentence")
                    )

    return out


def convert_local_birkbeck(path: Path) -> list[tuple[str, str, str, str, str]]:
    """本机离线适配器：读取 Birkbeck missp.dat（CC-BY-NC-SA-3.0，仅本机评测，严禁入库/入模型）。"""
    rows: list[tuple[str, str, str, str, str]] = []
    cur_target: str | None = None
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        s = raw.strip()
        if not s:
            continue
        if s.startswith("$"):
            cand = s[1:].strip().replace("_", " ")
            cur_target = cand if (" " not in cand and is_clean_token(cand)) else None
            continue
        if cur_target is None:
            continue
        typo = s.replace("_", " ").strip().lower()
        if " " in typo or not is_clean_token(typo):
            continue
        cat = "typo_phonetic" if is_phonetic_misspelling(typo, cur_target.lower()) else "typo_simple"
        if is_valid_sample(cat, "", typo, cur_target):
            rows.append((cat, "", typo, cur_target, "local-noncommercial: birkbeck"))
    return rows


def convert_local_jfleg(src_path: Path, ref_path: Path) -> list[tuple[str, str, str, str, str]]:
    """本机离线适配器：读取 JFLEG 源句与参考改写句（CC-BY-NC-SA-4.0，仅本机评测，严禁入库/入模型）。"""
    src_lines = src_path.read_text(encoding="utf-8", errors="replace").splitlines()
    ref_lines = ref_path.read_text(encoding="utf-8", errors="replace").splitlines()
    rows: list[tuple[str, str, str, str, str]] = []
    for s_raw, r_raw in zip(src_lines, ref_lines):
        s_toks = _clean_phrase_tokens(s_raw.replace(" ,", "").replace(",", ""))
        r_toks = _clean_phrase_tokens(r_raw.replace(" ,", "").replace(",", ""))
        if not s_toks or not r_toks:
            continue
        if not (4 <= len(s_toks) <= 14 and 4 <= len(r_toks) <= 14):
            continue
        inp = " ".join(s_toks)
        exp = " ".join(r_toks)
        if inp != exp and is_valid_sample("sentence", "", inp, exp):
            rows.append(("sentence", "", inp, exp, "local-noncommercial: jfleg"))
    return rows


def load_seed_tsv(path: Path) -> list[tuple[str, str, str, str, str]]:
    rows: list[tuple[str, str, str, str, str]] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        s = raw.strip()
        if not s or s.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) != 5:
            continue
        rows.append((parts[0].strip(), parts[1], parts[2], parts[3].strip(), parts[4].strip()))
    return rows


def write_eval_tsv(path: Path, rows: Iterable[tuple[str, str, str, str, str]], header_comment: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as fh:
        fh.write(f"# {header_comment}\n")
        fh.write("# Format: category<TAB>context<TAB>input<TAB>expected<TAB>notes\n")
        for cat, ctx, inp, exp, notes in rows:
            fh.write(f"{cat}\t{ctx}\t{inp}\t{exp}\t{notes}\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="从真实开源数据源构建大规模 AnIME 7 类离线评测集（支持防泄漏切分与本机非商用适配器）。"
    )
    parser.add_argument("--lexicon-tsv", type=Path, default=None, help="由 build_lexicon.py 生成的词库 TSV")
    parser.add_argument("--typos-csv", action="append", type=Path, default=[], help="crate-ci/typos CSV 路径")
    parser.add_argument("--wiki-raw", action="append", type=Path, default=[], help="Wikipedia For_machines 原始文本")
    parser.add_argument(
        "--github-typo-jsonl",
        action="append",
        type=Path,
        default=[],
        help="GitHub Typo Corpus (.jsonl / .jsonl.gz) 路径",
    )
    parser.add_argument("--multiwoz-dir", type=Path, default=None, help="MultiWOZ 2.2 JSON 目录（如 test/）")
    parser.add_argument("--multiwoz-json", action="append", type=Path, default=[], help="单个 MultiWOZ dialogues_*.json")
    parser.add_argument("--merge-seed-tsv", type=Path, default=None, help="优先保留的手工精校种子 TSV")
    parser.add_argument("--per-category", type=int, default=1500, help="每个类别目标样本上限（默认 1500）")
    parser.add_argument(
        "--split",
        choices=("all", "train", "eval"),
        default="eval",
        help="用于错拼/commit 的防泄漏切分（默认 eval）",
    )
    parser.add_argument("--eval-pct", type=int, default=20, help="eval 切分比例百分比（默认 20）")
    parser.add_argument(
        "--min-target-zipf",
        type=float,
        default=3.0,
        help="错拼与语境样本的目标词最低 Zipf 词频阈值（默认 3.0，过滤极生僻词）",
    )
    parser.add_argument("--seed", type=int, default=20261121, help="确定性抽样随机种子")
    parser.add_argument(
        "--out-tsv",
        type=Path,
        default=REPO_ROOT / "data" / "compiled" / "eval_large_open.tsv",
        help="输出开源基准 TSV 路径",
    )
    parser.add_argument("--stats-json", type=Path, default=None, help="可选统计 JSON 输出路径")
    # 本机非商用增强适配器（独立输出到 --local-out，绝不混入开源输出）
    parser.add_argument("--local-birkbeck", type=Path, default=None, help="[仅本机评测] Birkbeck missp.dat 路径")
    parser.add_argument("--local-jfleg-src", type=Path, default=None, help="[仅本机评测] JFLEG src 文件")
    parser.add_argument("--local-jfleg-ref", type=Path, default=None, help="[仅本机评测] JFLEG ref 文件")
    parser.add_argument(
        "--local-out",
        type=Path,
        default=REPO_ROOT / "data" / "compiled" / "eval_local_noncommercial.tsv",
        help="[仅本机评测] 非商用适配器输出 TSV 路径",
    )
    args = parser.parse_args(argv)

    # 若指定了本机非商用适配器模式，则仅生成独立的本机评测 TSV，确保零混用污染
    if args.local_birkbeck or args.local_jfleg_src or args.local_jfleg_ref:
        local_rows: list[tuple[str, str, str, str, str]] = []
        if args.local_birkbeck:
            local_rows.extend(convert_local_birkbeck(args.local_birkbeck))
        if args.local_jfleg_src or args.local_jfleg_ref:
            if not (args.local_jfleg_src and args.local_jfleg_ref):
                parser.error("--local-jfleg-src 与 --local-jfleg-ref 必须成对传入")
            local_rows.extend(convert_local_jfleg(args.local_jfleg_src, args.local_jfleg_ref))
        # 去重保持顺序
        seen_local: set[tuple[str, str, str]] = set()
        dedup_local = []
        for r in local_rows:
            k = (r[0], r[1], r[2])
            if k not in seen_local:
                seen_local.add(k)
                dedup_local.append(r)
        write_eval_tsv(
            args.local_out,
            dedup_local,
            "AnIME Local-Only Non-Commercial Eval Dataset (DO NOT COMMIT OR TRAIN)",
        )
        print(f"local non-commercial eval TSV written: {len(dedup_local)} rows -> {args.local_out}")
        return 0

    if not any(
        [
            args.lexicon_tsv,
            args.typos_csv,
            args.wiki_raw,
            args.github_typo_jsonl,
            args.multiwoz_dir,
            args.multiwoz_json,
            args.merge_seed_tsv,
        ]
    ):
        parser.error("至少需要提供一个开源输入源")

    rng = random.Random(args.seed)
    by_cat: dict[str, list[tuple[str, str, str, str, str]]] = {c: [] for c in CATEGORIES}
    seen_keys: set[tuple[str, str, str]] = set()

    def add_row(row: tuple[str, str, str, str, str]) -> bool:
        cat, ctx, inp, exp, _ = row
        if len(by_cat[cat]) >= args.per_category:
            return False
        k = (cat, ctx, inp)
        if k in seen_keys:
            return False
        if not is_valid_sample(cat, ctx, inp, exp):
            return False
        seen_keys.add(k)
        by_cat[cat].append(row)
        return True

    # 1. 先放入精校种子样本（如有）
    if args.merge_seed_tsv is not None:
        for row in load_seed_tsv(args.merge_seed_tsv):
            add_row(row)

    # 2. 读取词库与错拼表
    entries: list[tuple[str, float, int]] = []
    by_lower: dict[str, tuple[str, float]] = {}
    if args.lexicon_tsv is not None:
        entries, by_lower = load_lexicon_tsv(args.lexicon_tsv)

    wordlist_typos, reverse_typo = extract_typos_from_wordlists(
        wiki_paths=args.wiki_raw,
        typos_csv_paths=args.typos_csv,
        by_lower=by_lower,
        split=args.split,
        eval_pct=args.eval_pct,
        min_target_zipf=args.min_target_zipf,
    )
    rng.shuffle(wordlist_typos)

    # 3. 优先注入真实 GitHub Commit 错拼与 MultiWOZ 真实对话
    gh_extracted = extract_from_github_typo_corpus(
        jsonl_paths=args.github_typo_jsonl,
        by_lower=by_lower,
        split=args.split,
        eval_pct=args.eval_pct,
        max_per_cat=args.per_category,
    )
    for cat_rows in gh_extracted.values():
        rng.shuffle(cat_rows)

    multiwoz_files = list(args.multiwoz_json)
    if args.multiwoz_dir is not None:
        multiwoz_files.extend(sorted(args.multiwoz_dir.glob("dialogues_*.json")))
    mw_extracted = extract_from_multiwoz(
        multiwoz_files=multiwoz_files,
        by_lower=by_lower,
        reverse_typo=reverse_typo,
        rng=rng,
        max_per_cat=args.per_category,
        min_target_zipf=args.min_target_zipf,
    )

    # 交错合入真实数据源
    for row in gh_extracted.get("context_realword", []):
        add_row(row)
    for row in mw_extracted.get("context_realword", []):
        add_row(row)

    for row in gh_extracted.get("sentence", []):
        add_row(row)
    for row in mw_extracted.get("sentence", []):
        add_row(row)

    for row in wordlist_typos:
        add_row(row)
    for row in gh_extracted.get("typo_simple", []):
        add_row(row)
    for row in gh_extracted.get("typo_phonetic", []):
        add_row(row)

    # 4. 从词库补充 exact / prefix / typo_prefix（及在无外部错拼源时兜底）
    lex_samples = extract_lexicon_samples(
        entries=entries,
        rng=rng,
        exact_limit=args.per_category * 2,
        prefix_limit=args.per_category * 2,
        typo_prefix_limit=args.per_category * 2,
    )
    for cat in ("exact", "prefix", "typo_prefix"):
        for row in lex_samples[cat]:
            add_row(row)

    ordered_rows: list[tuple[str, str, str, str, str]] = []
    counts: dict[str, int] = {}
    for cat in CATEGORIES:
        counts[cat] = len(by_cat[cat])
        ordered_rows.extend(by_cat[cat])

    write_eval_tsv(
        args.out_tsv,
        ordered_rows,
        "AnIME Open-Source Evaluation Benchmark (7 categories per .agents/contracts/eval-schema.md)",
    )

    if args.stats_json is not None:
        args.stats_json.parent.mkdir(parents=True, exist_ok=True)
        args.stats_json.write_text(
            json.dumps({"total": len(ordered_rows), "categories": counts}, indent=2),
            encoding="utf-8",
        )

    print(f"eval benchmark build complete: total={len(ordered_rows)} counts={counts} -> {args.out_tsv}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
