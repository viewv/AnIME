#!/usr/bin/env python3
"""AnIME 评测 v3 构建器：更大且含多错误的 noise_v2、LibriSpeech 留出跨领域集 ood_v1，以及经复核的干净反例。

- noise_v2：MultiWOZ 2.2 test 子句，`build_eval_v2` 的 8 类单错误算子各 N 条，外加 `noise:multi`
  （同句 2–3 处不同类型错误），以及 `negative:clean` 反例。
- ood_v1：LibriSpeech LM 语料按 CRC32 确定性抽取的留出句（文学领域，非 MultiWOZ），同样包含
  干净反例、单错误与多错误样本。该文件位于 `eval/` 下，`build_ngram_kn.py` 会自动把这些句子及其
  5-gram 从 L2 训练语料中剔除；新增后必须重建 L2。
- 反例复核：`--review-file` 为人工裁决表（`sentence<TAB>exclude|keep<TAB>reason`），标为 exclude 的
  源句（自身有拼写/语法错误）不进入任何集合。裁决流程：先用 `--emit-negative-candidates` 导出候选，
  以当前引擎评测，只复核引擎改动了的句子（分歧裁决），未被改动的候选直接保留。

源句选择与噪声均由固定 seed 决定，可完全复现。
"""

from __future__ import annotations

import argparse
import gzip
import random
import sys
import zlib
from collections import Counter
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from build_eval_benchmark import ALPHA_WORD_RE, is_valid_sample  # noqa: E402
from build_eval_v2 import (  # noqa: E402
    DROPPED_APOSTROPHE_FORMS,
    MIN_SOURCE_ZIPF,
    NOISE_TYPES,
    SENTENCE_NOISERS,
    Row,
    extract_clauses,
    load_lexicon,
    load_realword_vocab,
    realword_edit_sample,
    write_tsv,
)

# 留出比例：每 10 万行取约 40 行（全量 4041 万行约 1.6 万候选），只抽字母序无关的哈希桶
HOLDOUT_BUCKET_PER_100K = 40
MULTI_OPS = ("leading_drop", "boundary_gemination", "space_shift", "space_extra", "space_missing", "adjacent_key")


def load_review(path: Path | None) -> set[str]:
    excluded: set[str] = set()
    if path is None or not path.exists():
        return excluded
    for raw in path.read_text(encoding="utf-8").splitlines():
        if not raw.strip() or raw.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) >= 2 and parts[1].strip() == "exclude":
            excluded.add(parts[0].strip().lower())
    return excluded


def librispeech_holdout_clauses(
    gz_path: Path, lex: dict[str, tuple[str, float]], bucket: int = HOLDOUT_BUCKET_PER_100K
) -> list[list[str]]:
    """全量扫描（语料按字母序排序，截断会只取到 A 开头的句子），按 CRC32 桶抽取 4–12 词常见词句子。"""
    out: list[list[str]] = []
    seen: set[str] = set()
    with gzip.open(gz_path, "rt", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line or zlib.crc32(line.encode()) % 100_000 >= bucket:
                continue
            toks = line.lower().split()
            if not (4 <= len(toks) <= 12):
                continue
            norm: list[str] = []
            ok = True
            for t in toks:
                hit = lex.get(t)
                if hit is None or hit[1] < MIN_SOURCE_ZIPF or not ALPHA_WORD_RE.match(t):
                    ok = False
                    break
                if t in DROPPED_APOSTROPHE_FORMS:
                    ok = False
                    break
                norm.append(hit[0])
            key = " ".join(norm).lower()
            if ok and key not in seen:
                seen.add(key)
                out.append(norm)
    return out


def noise_multi(toks: list[str], rng: random.Random, lex) -> str | None:
    """同句依次施加 2–3 种不同的单错误算子（每次作用在上一步结果上）。"""
    k = rng.choice((2, 2, 3))
    ops = rng.sample(MULTI_OPS, k)
    cur = [t for t in toks]
    applied = 0
    for op in ops:
        res = SENTENCE_NOISERS[op](cur, rng, lex)
        if res:
            cur = res.split(" ")
            applied += 1
    out = " ".join(cur)
    # 先合并后拆分等组合可能互相抵消，抵消后不算多错误样本
    if applied < 2 or out.lower() == " ".join(toks).lower():
        return None
    return out


def build_rows(
    clauses: list[list[str]],
    lex,
    realword_vocab: set[str],
    rng: random.Random,
    per_type: int,
    multi: int,
    negatives: int,
    excluded: set[str],
) -> tuple[list[Row], Counter, list[str]]:
    rows: list[Row] = []
    stats: Counter = Counter()
    seen: set[tuple[str, str]] = set()
    usable = [c for c in clauses if " ".join(c).lower() not in excluded]
    stats["excluded_by_review"] = len(clauses) - len(usable)

    def take(ntype: str, quota: int, make) -> None:
        order = list(range(len(usable)))
        rng.shuffle(order)
        for ci in order:
            if stats[ntype] >= quota:
                return
            row = make(usable[ci])
            if row is None or not is_valid_sample(row[0], row[1], row[2], row[3]):
                continue
            if (row[1], row[2]) in seen:
                continue
            seen.add((row[1], row[2]))
            rows.append(row)
            stats[ntype] += 1

    for ntype in NOISE_TYPES:
        if ntype == "realword_edit":
            take(ntype, per_type, lambda t: realword_edit_sample(t, rng, realword_vocab))
            continue

        def make(t, ntype=ntype):
            noisy = SENTENCE_NOISERS[ntype](t, rng, lex)
            exp = " ".join(t)
            if not noisy or noisy.lower() == exp.lower():
                return None
            return ("sentence", "", noisy, exp, f"noise:{ntype}")

        take(ntype, per_type, make)

    def make_multi(t):
        noisy = noise_multi(t, rng, lex)
        exp = " ".join(t)
        if not noisy or noisy.lower() == exp.lower():
            return None
        return ("sentence", "", noisy, exp, "noise:multi")

    take("multi", multi, make_multi)

    candidates: list[str] = []

    def make_neg(t):
        sent = " ".join(t)
        candidates.append(sent)
        return ("sentence", "", sent, sent, "negative:clean")

    take("negative_clean", negatives, make_neg)
    return rows, stats, candidates


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--lexicon-tsv", type=Path, default=REPO_ROOT / "data/compiled/lexicon.tsv")
    ap.add_argument("--multiwoz-json", type=Path, action="append", default=[])
    ap.add_argument("--librispeech", type=Path, default=REPO_ROOT / "data/raw/ngram-corpus/librispeech-lm-norm.txt.gz")
    ap.add_argument("--review-file", type=Path, default=REPO_ROOT / "eval/review/negative_review.tsv")
    ap.add_argument("--out-noise", type=Path, default=REPO_ROOT / "eval/benchmarks/noise_v2.tsv")
    ap.add_argument("--out-ood", type=Path, default=REPO_ROOT / "eval/benchmarks/ood_v1.tsv")
    ap.add_argument("--emit-negative-candidates", type=Path, default=None,
                    help="额外导出全部干净候选（未经复核）为评测 TSV，用于分歧裁决")
    ap.add_argument("--per-type", type=int, default=150)
    ap.add_argument("--multi", type=int, default=150)
    ap.add_argument("--negatives", type=int, default=300)
    ap.add_argument("--ood-per-type", type=int, default=40)
    ap.add_argument("--ood-multi", type=int, default=80)
    ap.add_argument("--ood-negatives", type=int, default=200)
    ap.add_argument("--seed", type=int, default=20261123)
    args = ap.parse_args(argv)

    if not args.multiwoz_json:
        args.multiwoz_json = sorted((REPO_ROOT / "data/raw").glob("multiwoz-2.2-test-*/dialogues_*.json"))
    lex = load_lexicon(args.lexicon_tsv)
    realword_vocab = load_realword_vocab(args.lexicon_tsv)
    excluded = load_review(args.review_file)

    woz = extract_clauses(args.multiwoz_json, lex)
    libri = librispeech_holdout_clauses(args.librispeech, lex)
    print(f"source clauses: multiwoz={len(woz)} librispeech_holdout={len(libri)} review_excluded={len(excluded)}")

    noise_rows, noise_stats, woz_cands = build_rows(
        woz, lex, realword_vocab, random.Random(args.seed), args.per_type, args.multi, args.negatives, excluded
    )
    ood_rows, ood_stats, libri_cands = build_rows(
        libri, lex, realword_vocab, random.Random(args.seed + 1), args.ood_per_type, args.ood_multi,
        args.ood_negatives, excluded
    )
    header_common = f"seed={args.seed}；由 scripts/data_pipeline/build_eval_v3.py 生成（勿手改），反例经 eval/review/negative_review.tsv 复核"
    write_tsv(args.out_noise, noise_rows, [
        "AnIME noise_v2：MultiWOZ 2.2 test 子句，8 类单错误 + noise:multi 多错误 + 复核后的 negative:clean",
        header_common,
    ])
    write_tsv(args.out_ood, ood_rows, [
        "AnIME ood_v1：LibriSpeech LM 语料 CRC32 留出句（文学领域）；须经 build_ngram_kn.py 去泄漏后重建 L2",
        header_common,
    ])
    if args.emit_negative_candidates:
        cands = [("sentence", "", s, s, "negative:clean") for s in woz_cands + libri_cands]
        write_tsv(args.emit_negative_candidates, cands, ["未复核的干净反例候选（仅用于分歧裁决，勿入库）"])
    print(f"noise_v2: {len(noise_rows)} rows {dict(sorted(noise_stats.items()))}")
    print(f"ood_v1: {len(ood_rows)} rows {dict(sorted(ood_stats.items()))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
