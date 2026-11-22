#!/usr/bin/env python3
"""AnIME 评测集 v2 构建器：由冻结的 core_v1 派生 core_v2，并从 MultiWOZ 2.2 test 合成真实打字噪声集 noise_v1。

- core_v2：`prefix` 剔除输入本身即词典词的歧义样本；`prefix` / `typo_prefix` 的参考答案扩展为同词元屈折变体
  （`grenade || grenades`），其余类别原样保留。
- noise_v1：每条样本只注入**一种**噪声并以 `noise:<type>` 打标签，另附 `negative:clean` 干净整句反例。
  源句全部取自 MultiWOZ 2.2 **test** 分片（与 core_v1 同源），训练语料必须用 `eval_leak_filter` 排除。

格式与标签约定见 `.agents/contracts/eval-schema.md` §1。
"""

from __future__ import annotations

import argparse
import json
import random
import re
import sys
from collections import Counter
from pathlib import Path
from typing import Callable, Iterable

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from common import ENTRY_FLAG_FROM_SCOWL  # noqa: E402
from build_eval_benchmark import (  # noqa: E402
    ALPHA_WORD_RE,
    APOSTROPHE_DROPS,
    CAMEL_OR_ALLCAPS_RE,
    QWERTY_NEIGHBORS,
    is_valid_sample,
)

Row = tuple[str, str, str, str, str]

# APOSTROPHE_DROPS 为 "i'm" -> "im" 的映射；其值即漏撇号写法
DROPPED_APOSTROPHE_FORMS = frozenset(APOSTROPHE_DROPS.values())

NOISE_TYPES = (
    "leading_drop",
    "boundary_gemination",
    "space_shift",
    "space_extra",
    "space_missing",
    "adjacent_key",
    "realword_edit",
    "contraction",
)

# 屈折变体只从"本身是常见词"的词干展开，避免 this -> thi -> thing 这类伪词干污染参考集
MIN_STEM_ZIPF = 2.5
MIN_FORM_ZIPF = 2.0
# 真词编辑只产出常见词，模拟"打成了另一个真实存在的词"（that -> hat、from -> form）
MIN_REALWORD_ZIPF = 3.5
MIN_SOURCE_ZIPF = 3.0


def load_lexicon(path: Path) -> dict[str, tuple[str, float]]:
    """读取 build_lexicon.py 产出的 TSV：小写词 -> (规范写法, zipf)。"""
    by_lower: dict[str, tuple[str, float]] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        if not raw or raw.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) < 2:
            continue
        word = parts[0]
        try:
            zipf = float(parts[1])
        except ValueError:
            continue
        low = word.lower()
        if low not in by_lower or by_lower[low][1] < zipf:
            by_lower[low] = (word, zipf)
    return by_lower


def _zipf(lex: dict[str, tuple[str, float]], w: str) -> float:
    hit = lex.get(w.lower())
    return hit[1] if hit else -1.0


def inflection_variants(word: str, lex: dict[str, tuple[str, float]]) -> list[str]:
    """同词元的规则屈折变体（复数 / 第三人称 / -ed / -ing），仅返回词典中存在的其他词形。"""
    lw = word.lower()
    if not lw.isalpha() or len(lw) < 3:
        return []
    stems = {lw}
    for suf in ("ies", "ied", "es", "s", "ed", "d", "ing"):
        if lw.endswith(suf) and len(lw) - len(suf) >= 3:
            base = lw[: -len(suf)]
            stems.add(base)
            if suf in ("ies", "ied"):
                stems.add(base + "y")
            if suf in ("ed", "ing"):
                stems.add(base + "e")
                if len(base) >= 2 and base[-1] == base[-2]:
                    stems.add(base[:-1])
    stems = {s for s in stems if s == lw or _zipf(lex, s) >= MIN_STEM_ZIPF}

    forms: set[str] = set()
    for st in stems:
        forms.update({st, st + "s", st + "es", st + "ed", st + "d", st + "ing"})
        if st.endswith("y") and len(st) > 3:
            forms.update({st[:-1] + "ies", st[:-1] + "ied"})
        if st.endswith("e"):
            forms.add(st[:-1] + "ing")
        if len(st) >= 3 and st[-1] not in "aeiouwxy" and st[-2] in "aeiou" and st[-3] not in "aeiou":
            forms.update({st + st[-1] + "ed", st + st[-1] + "ing"})
    out = [lex[f][0] for f in forms if f != lw and _zipf(lex, f) >= MIN_FORM_ZIPF]
    return sorted(set(out), key=lambda w: (-_zipf(lex, w), w))


def read_tsv_rows(path: Path) -> list[Row]:
    rows: list[Row] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) != 5:
            raise ValueError(f"{path}: malformed row {raw!r}")
        rows.append((parts[0], parts[1], parts[2], parts[3], parts[4]))
    return rows


def derive_core_v2(rows: Iterable[Row], lex: dict[str, tuple[str, float]]) -> tuple[list[Row], Counter]:
    stats: Counter = Counter()
    out: list[Row] = []
    for cat, ctx, inp, exp, notes in rows:
        if cat == "prefix" and inp.lower() in lex:
            stats["prefix_dropped_input_is_word"] += 1
            continue
        if cat in ("prefix", "typo_prefix") and " " not in exp:
            variants = inflection_variants(exp, lex)
            if cat == "prefix":
                variants = [v for v in variants if v.lower().startswith(inp.lower()) and len(v) > len(inp)]
            if variants:
                stats[f"{cat}_multi_ref"] += 1
                exp = " || ".join([exp] + variants)
        out.append((cat, ctx, inp, exp, notes))
        stats[f"kept_{cat}"] += 1
    return out, stats


def extract_clauses(multiwoz_files: list[Path], lex: dict[str, tuple[str, float]]) -> list[list[str]]:
    """抽取 MultiWOZ USER 话轮中 4–10 词、全部为常见词典词的子句。"""
    texts: list[str] = []
    for path in sorted(multiwoz_files):
        data = json.loads(path.read_text(encoding="utf-8"))
        for dlg in data if isinstance(data, list) else []:
            for turn in dlg.get("turns", []):
                if turn.get("speaker") == "USER":
                    texts.append(str(turn.get("utterance", "")).strip())
    return clauses_from_texts(texts, lex)


def clauses_from_texts(texts: list[str], lex: dict[str, tuple[str, float]]) -> list[list[str]]:
    """从话轮文本抽取 4–10 词、全部为常见词典词的子句，规范大小写（句首保留原样），全局去重。"""
    clauses: list[list[str]] = []
    seen: set[str] = set()
    for utt in texts:
        for seg in [utt] + [s.strip() for s in re.split(r"[.!?,;]+", utt)]:
            toks = seg.strip().rstrip(".!?").split()
            if not (4 <= len(toks) <= 10):
                continue
            if any(not ALPHA_WORD_RE.match(t) or CAMEL_OR_ALLCAPS_RE.search(t) for t in toks):
                continue
            # 源句自带漏撇号（Im / dont）时参考答案本身就是错拼，不能作为干净参考
            if any(t.lower() in DROPPED_APOSTROPHE_FORMS for t in toks):
                continue
            norm: list[str] = []
            ok = True
            for i, t in enumerate(toks):
                hit = lex.get(t.lower())
                if hit is None or hit[1] < MIN_SOURCE_ZIPF:
                    ok = False
                    break
                canon = hit[0]
                if i == 0 and t[0].isupper():
                    norm.append(t[0] + canon[1:] if canon.lower() == t.lower() else t)
                elif canon[0].isupper() or canon == "I":
                    norm.append(canon)
                else:
                    norm.append(canon)
            if not ok:
                continue
            key = " ".join(norm).lower()
            if key not in seen:
                seen.add(key)
                clauses.append(norm)
    return clauses


def _alpha(w: str) -> bool:
    return w.isalpha()


def noise_leading_drop(toks: list[str], rng: random.Random, lex) -> str | None:
    idx = [i for i, w in enumerate(toks) if _alpha(w) and 2 <= len(w) <= 6 and w.lower() not in ("a", "i")]
    if not idx:
        return None
    i = rng.choice(idx)
    out = [w.lower() for w in toks]
    out[i] = out[i][1:]
    return " ".join(out)


def noise_boundary_gemination(toks: list[str], rng: random.Random, lex) -> str | None:
    """相邻两词首尾同字母时快打只按一次（want to -> want o）。"""
    idx = [
        i
        for i in range(len(toks) - 1)
        if _alpha(toks[i]) and _alpha(toks[i + 1]) and len(toks[i + 1]) >= 2
        and toks[i][-1].lower() == toks[i + 1][0].lower()
    ]
    if not idx:
        return None
    i = rng.choice(idx)
    out = [w.lower() for w in toks]
    out[i + 1] = out[i + 1][1:]
    return " ".join(out)


def noise_space_shift(toks: list[str], rng: random.Random, lex) -> str | None:
    """空格左移或右移一个字符（want to -> wan tto / wantt o）。"""
    idx = [i for i in range(len(toks) - 1) if _alpha(toks[i]) and _alpha(toks[i + 1])
           and len(toks[i]) >= 3 and len(toks[i + 1]) >= 3]
    if not idx:
        return None
    i = rng.choice(idx)
    out = [w.lower() for w in toks]
    a, b = out[i], out[i + 1]
    if rng.random() < 0.5:
        out[i], out[i + 1] = a[:-1], a[-1] + b
    else:
        out[i], out[i + 1] = a + b[0], b[1:]
    return " ".join(out)


def noise_space_extra(toks: list[str], rng: random.Random, lex) -> str | None:
    idx = [i for i, w in enumerate(toks) if _alpha(w) and len(w) >= 5]
    if not idx:
        return None
    i = rng.choice(idx)
    out = [w.lower() for w in toks]
    w = out[i]
    pos = rng.randint(2, len(w) - 2)
    out[i] = w[:pos] + " " + w[pos:]
    return " ".join(out)


def noise_space_missing(toks: list[str], rng: random.Random, lex) -> str | None:
    idx = [i for i in range(len(toks) - 1) if _alpha(toks[i]) and _alpha(toks[i + 1])]
    if not idx:
        return None
    i = rng.choice(idx)
    out = [w.lower() for w in toks]
    out[i : i + 2] = [out[i] + out[i + 1]]
    return " ".join(out)


def noise_adjacent_key(toks: list[str], rng: random.Random, lex) -> str | None:
    idx = [i for i, w in enumerate(toks) if _alpha(w) and len(w) >= 3]
    if not idx:
        return None
    i = rng.choice(idx)
    out = [w.lower() for w in toks]
    w = out[i]
    pos = rng.randrange(len(w))
    nbrs = QWERTY_NEIGHBORS.get(w[pos], "")
    if not nbrs:
        return None
    cand = w[:pos] + rng.choice(nbrs) + w[pos + 1 :]
    if cand in lex:
        return None  # 撞上真词归 realword_edit，保持标签纯净
    out[i] = cand
    return " ".join(out)


def single_edits(word: str) -> set[str]:
    """一次删除 / QWERTY 邻键替换 / 相邻换位 / 邻键插入。"""
    w = word.lower()
    out: set[str] = set()
    for p in range(len(w)):
        out.add(w[:p] + w[p + 1 :])
        for n in QWERTY_NEIGHBORS.get(w[p], ""):
            out.add(w[:p] + n + w[p + 1 :])
            out.add(w[:p] + n + w[p:])
        if p + 1 < len(w) and w[p] != w[p + 1]:
            out.add(w[:p] + w[p + 1] + w[p] + w[p + 2 :])
    out.discard(w)
    out.discard("")
    return out


# 两字母"词"里大量是缩写（st、al、mr、ie），只接受真实功能词
TWO_LETTER_REALWORDS = frozenset(
    "am an as at be by do go he if in is it me my no of on or so to up us we".split()
)


def load_realword_vocab(path: Path) -> set[str]:
    """真词编辑的目标词：SCOWL 收录、非专名大写、两字母限功能词，排除 goin/somethin 这类口语拼写。"""
    vocab: set[str] = set()
    for raw in path.read_text(encoding="utf-8").splitlines():
        if not raw or raw.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) < 4:
            continue
        word, zipf, flags = parts[0], parts[1], parts[3]
        try:
            zipf_f, flags_i = float(zipf), int(flags)
        except ValueError:
            continue
        if not word.isalpha() or not word.islower() or not flags_i & ENTRY_FLAG_FROM_SCOWL:
            continue
        if zipf_f < MIN_REALWORD_ZIPF or (len(word) <= 2 and word not in TWO_LETTER_REALWORDS):
            continue
        vocab.add(word)
    return vocab


def realword_edit_sample(toks: list[str], rng: random.Random, realword_vocab: set[str]) -> Row | None:
    """把句中一个词打成另一个常见真词，作为依赖上文的 context_realword 样本。"""
    order = list(range(2, len(toks)))
    rng.shuffle(order)
    for i in order:
        w = toks[i]
        if not _alpha(w) or len(w) < 2:
            continue
        cands = sorted(e for e in single_edits(w) if e in realword_vocab)
        if not cands:
            continue
        wrong = rng.choice(cands)
        ctx = " ".join(toks[:i]) + " "
        inp = " ".join([wrong] + [t.lower() for t in toks[i + 1 :]])
        exp = " ".join(toks[i:])
        return ("context_realword", ctx, inp, exp, f"noise:realword_edit {w}->{wrong}")
    return None


def noise_contraction(toks: list[str], rng: random.Random, lex) -> str | None:
    idx = [i for i, w in enumerate(toks) if "'" in w]
    if not idx:
        return None
    out = [w.lower() for w in toks]
    for i in idx:
        out[i] = out[i].replace("'", "")
    return " ".join(out)


SENTENCE_NOISERS: dict[str, Callable] = {
    "leading_drop": noise_leading_drop,
    "boundary_gemination": noise_boundary_gemination,
    "space_shift": noise_space_shift,
    "space_extra": noise_space_extra,
    "space_missing": noise_space_missing,
    "adjacent_key": noise_adjacent_key,
    "contraction": noise_contraction,
}


def build_noise_rows(
    clauses: list[list[str]],
    lex: dict[str, tuple[str, float]],
    rng: random.Random,
    per_type: int,
    negatives: int,
    realword_vocab: set[str],
) -> tuple[list[Row], Counter]:
    rows: list[Row] = []
    stats: Counter = Counter()
    seen_inputs: set[tuple[str, str]] = set()

    for ntype in NOISE_TYPES:
        order = list(range(len(clauses)))
        rng.shuffle(order)
        for ci in order:
            if stats[ntype] >= per_type:
                break
            toks = clauses[ci]
            if ntype == "realword_edit":
                row = realword_edit_sample(toks, rng, realword_vocab)
            else:
                noisy = SENTENCE_NOISERS[ntype](toks, rng, lex)
                exp = " ".join(toks)
                row = None
                if noisy and noisy.lower() != exp.lower():
                    row = ("sentence", "", noisy, exp, f"noise:{ntype}")
            if row is None or not is_valid_sample(row[0], row[1], row[2], row[3]):
                continue
            key = (row[1], row[2])
            if key in seen_inputs:
                continue
            seen_inputs.add(key)
            rows.append(row)
            stats[ntype] += 1

    order = list(range(len(clauses)))
    rng.shuffle(order)
    for ci in order:
        if stats["negative_clean"] >= negatives:
            break
        sent = " ".join(clauses[ci])
        if ("", sent) in seen_inputs or not is_valid_sample("sentence", "", sent, sent):
            continue
        seen_inputs.add(("", sent))
        rows.append(("sentence", "", sent, sent, "negative:clean"))
        stats["negative_clean"] += 1
    return rows, stats


def write_tsv(path: Path, rows: Iterable[Row], header: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        for line in header:
            f.write(f"# {line}\n")
        f.write("# category\tcontext\tinput\texpected\tnotes\n")
        for r in rows:
            f.write("\t".join(r) + "\n")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--lexicon-tsv", type=Path, default=REPO_ROOT / "data/compiled/lexicon.tsv")
    ap.add_argument("--core-v1", type=Path, default=REPO_ROOT / "eval/benchmarks/core_v1.tsv")
    ap.add_argument("--multiwoz-json", type=Path, action="append", default=[])
    ap.add_argument("--out-core-v2", type=Path, default=REPO_ROOT / "eval/benchmarks/core_v2.tsv")
    ap.add_argument("--out-noise", type=Path, default=REPO_ROOT / "eval/benchmarks/noise_v1.tsv")
    ap.add_argument("--per-type", type=int, default=60)
    ap.add_argument("--negatives", type=int, default=200)
    ap.add_argument("--seed", type=int, default=20261122)
    args = ap.parse_args(argv)

    if not args.multiwoz_json:
        args.multiwoz_json = sorted((REPO_ROOT / "data/raw").glob("multiwoz-2.2-test-*/dialogues_*.json"))
    if not args.multiwoz_json:
        print("no MultiWOZ test JSON found (run fetch_sources.py first)", file=sys.stderr)
        return 1

    lex = load_lexicon(args.lexicon_tsv)
    core_v2, core_stats = derive_core_v2(read_tsv_rows(args.core_v1), lex)
    write_tsv(
        args.out_core_v2,
        core_v2,
        [
            "AnIME core_v2：由冻结的 core_v1 经 scripts/data_pipeline/build_eval_v2.py 派生（勿手改）",
            "prefix 剔除输入本身即词典词的歧义样本；prefix/typo_prefix 参考扩展为同词元屈折变体（|| 分隔）",
        ],
    )

    rng = random.Random(args.seed)
    clauses = extract_clauses(args.multiwoz_json, lex)
    noise_rows, noise_stats = build_noise_rows(
        clauses, lex, rng, args.per_type, args.negatives, load_realword_vocab(args.lexicon_tsv)
    )
    write_tsv(
        args.out_noise,
        noise_rows,
        [
            "AnIME noise_v1：MultiWOZ 2.2 test 子句注入单一真实打字噪声，由 scripts/data_pipeline/build_eval_v2.py 生成（勿手改）",
            f"seed={args.seed} per_type={args.per_type} negatives={args.negatives}；源句不得进入任何训练语料",
        ],
    )

    print(f"core_v2: {len(core_v2)} rows {dict(sorted(core_stats.items()))}")
    print(f"noise_v1: {len(noise_rows)} rows from {len(clauses)} clauses {dict(sorted(noise_stats.items()))}")
    short = [t for t in NOISE_TYPES if noise_stats[t] < args.per_type]
    if short:
        print(f"WARNING: noise types below target: {short}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
