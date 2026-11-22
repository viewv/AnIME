#!/usr/bin/env python3
"""AnIME 评测 v3 构建器：更大且含多错误的 noise_v2、LibriSpeech 留出跨领域集 ood_v1，以及经复核的干净反例。

- noise_v2：MultiWOZ 2.2 test 子句，`build_eval_v2` 的 8 类单错误算子各 N 条，外加 `noise:multi`
  （同句 2–3 处不同类型错误），以及 `negative:clean` 反例。
- ood_v1：LibriSpeech LM 语料按 CRC32 确定性抽取的留出句（文学领域，非 MultiWOZ），同样包含
  干净反例、单错误与多错误样本。该文件位于 `eval/` 下，`build_ngram_kn.py` 会自动把这些句子及其
  5-gram 从 L2 训练语料中剔除；新增后必须重建 L2。
- ood_tm2_v1：Google Taskmaster-2（CC BY 4.0）food-ordering / music / sports / movies 领域 USER 话轮，
  日常口语任务对话（非 MultiWOZ 订酒店领域）；同样包含干净反例、单错误与多错误样本。
- 反例复核：`--review-file` 为人工裁决表（`sentence<TAB>exclude|keep<TAB>reason`），标为 exclude 的
  源句（自身有拼写/语法错误）不进入任何集合。裁决流程：先用 `--emit-negative-candidates` 导出候选，
  以当前引擎评测，只复核引擎改动了的句子（分歧裁决），未被改动的候选直接保留。

源句选择与噪声均由固定 seed 决定，可完全复现。
"""

from __future__ import annotations

import argparse
import gzip
import json
import random
import re
import sys
import zlib
from collections import Counter
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from build_eval_benchmark import ALPHA_WORD_RE, is_valid_sample  # noqa: E402
from build_oasst_text import DEFAULT_IN as OASST_IN, heldout_prompter_paragraphs  # noqa: E402
from build_eval_v2 import (  # noqa: E402
    DROPPED_APOSTROPHE_FORMS,
    clauses_from_texts,
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


def taskmaster_user_texts(paths: list[Path]) -> list[str]:
    texts: list[str] = []
    for p in sorted(paths):
        data = json.loads(p.read_text(encoding="utf-8"))
        for dlg in data if isinstance(data, list) else []:
            for utt in dlg.get("utterances", []):
                if utt.get("speaker") == "USER":
                    texts.append(" ".join(str(utt.get("text", "")).split()))
    return texts


# 开发者风格源句：比 MultiWOZ 句子更长更杂，放宽到 14 词；词频低于该门槛或不在词典的字母词视为"新词"
DEV_MIN_WORDS, DEV_MAX_WORDS = 4, 14
DEV_KNOWN_MIN_ZIPF = 2.5
DEV_MAX_NOVEL_TOKENS = 2
_DEV_SPLIT_RE = re.compile(r"[.!?,;:()\"]+")


def dev_clauses(texts: list[str], lex) -> tuple[list[list[str]], list[list[str]]]:
    """切出 (全部为常见词典词的子句, 含 1–2 个词典外新词的子句)；新词保留原始大小写，不参与加噪。"""
    standard: list[list[str]] = []
    novel: list[list[str]] = []
    seen: set[str] = set()
    for utt in texts:
        for seg in [utt] + [x.strip() for x in _DEV_SPLIT_RE.split(utt)]:
            toks = seg.strip().rstrip(".!?").split()
            if not (DEV_MIN_WORDS <= len(toks) <= DEV_MAX_WORDS):
                continue
            if any(not ALPHA_WORD_RE.match(t) or t.lower() in DROPPED_APOSTROPHE_FORMS for t in toks):
                continue
            norm: list[str] = []
            n_novel = 0
            for i, t in enumerate(toks):
                if t.lower() == "i":
                    # 词典 TSV 不收录代词 I（引擎以缩写规则处理 i -> I），不能把它当新词
                    norm.append("I")
                    continue
                hit = lex.get(t.lower())
                if hit is not None and hit[1] >= DEV_KNOWN_MIN_ZIPF:
                    canon = hit[0]
                    norm.append(t[0] + canon[1:] if i == 0 and t[0].isupper() and canon.lower() == t.lower() else canon)
                else:
                    n_novel += 1
                    norm.append(t)
            key = " ".join(norm).lower()
            if key in seen or n_novel > DEV_MAX_NOVEL_TOKENS:
                continue
            seen.add(key)
            (standard if n_novel == 0 else novel).append(norm)
    return standard, novel


def is_valid_novel_sample(sent: str) -> bool:
    # 新词句允许驼峰 / 全大写（JavaScript、API），只做最基本的格式校验
    return bool(sent) and "\t" not in sent and "  " not in sent and sent == sent.strip() and sent[0].isalpha()


def build_novel_negatives(clauses: list[list[str]], quota: int, excluded: set[str], seed_tag: str) -> list[Row]:
    order = sorted(clauses, key=lambda c: zlib.crc32(f"{seed_tag}:novel:{' '.join(c).lower()}".encode()))
    rows: list[Row] = []
    for c in order:
        if len(rows) >= quota:
            break
        sent = " ".join(c)
        if sent.lower() in excluded or not is_valid_novel_sample(sent):
            continue
        rows.append(("sentence", "", sent, sent, "negative:novel"))
    return rows


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
    seed_tag: str = "",
) -> tuple[list[Row], Counter, list[str]]:
    rows: list[Row] = []
    stats: Counter = Counter()
    seen: set[tuple[str, str]] = set()
    usable = [c for c in clauses if " ".join(c).lower() not in excluded]
    stats["excluded_by_review"] = len(clauses) - len(usable)

    def take(ntype: str, quota: int, make) -> None:
        # 按 (seed, 类型, 句子) 的 CRC32 稳定排序取样：复核剔除一句只会顺延下一句，
        # 不会像随机洗牌那样重抽整个样本，复核才能收敛
        order = sorted(
            range(len(usable)),
            key=lambda i: zlib.crc32(f"{seed_tag}:{ntype}:{' '.join(usable[i]).lower()}".encode()),
        )
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
    ap.add_argument("--taskmaster-json", type=Path, action="append", default=[])
    ap.add_argument("--oasst", type=Path, default=OASST_IN)
    ap.add_argument("--out-ood-dev", type=Path, default=REPO_ROOT / "eval/benchmarks/ood_dev_v1.tsv")
    ap.add_argument("--dev-per-type", type=int, default=60)
    ap.add_argument("--dev-multi", type=int, default=100)
    ap.add_argument("--dev-negatives", type=int, default=300)
    ap.add_argument("--dev-novel", type=int, default=150)
    ap.add_argument("--out-ood-tm2", type=Path, default=REPO_ROOT / "eval/benchmarks/ood_tm2_v1.tsv")
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

    if not args.taskmaster_json:
        args.taskmaster_json = sorted((REPO_ROOT / "data/raw").glob("taskmaster-2-*/*.json"))
    woz = extract_clauses(args.multiwoz_json, lex)
    tm2 = clauses_from_texts(taskmaster_user_texts(args.taskmaster_json), lex)
    dev_std, dev_novel = dev_clauses(heldout_prompter_paragraphs(args.oasst), lex) if args.oasst.exists() else ([], [])
    libri = librispeech_holdout_clauses(args.librispeech, lex)
    print(f"source clauses: multiwoz={len(woz)} librispeech_holdout={len(libri)} taskmaster2={len(tm2)} oasst_dev={len(dev_std)}+{len(dev_novel)}novel review_excluded={len(excluded)}")

    noise_rows, noise_stats, woz_cands = build_rows(
        woz, lex, realword_vocab, random.Random(args.seed), args.per_type, args.multi, args.negatives, excluded,
        seed_tag=f"{args.seed}:woz",
    )
    ood_rows, ood_stats, libri_cands = build_rows(
        libri, lex, realword_vocab, random.Random(args.seed + 1), args.ood_per_type, args.ood_multi,
        args.ood_negatives, excluded, seed_tag=f"{args.seed}:libri",
    )
    tm2_rows, tm2_stats, tm2_cands = build_rows(
        tm2, lex, realword_vocab, random.Random(args.seed + 2), args.ood_per_type, args.ood_multi,
        args.ood_negatives, excluded, seed_tag=f"{args.seed}:tm2",
    )
    dev_rows, dev_stats, dev_cands = build_rows(
        dev_std, lex, realword_vocab, random.Random(args.seed + 3), args.dev_per_type, args.dev_multi,
        args.dev_negatives, excluded, seed_tag=f"{args.seed}:dev",
    )
    novel_rows = build_novel_negatives(dev_novel, args.dev_novel, excluded, f"{args.seed}:dev")
    dev_stats["negative_novel"] = len(novel_rows)
    header_common = f"seed={args.seed}；由 scripts/data_pipeline/build_eval_v3.py 生成（勿手改），反例经 eval/review/negative_review.tsv 复核"
    write_tsv(args.out_noise, noise_rows, [
        "AnIME noise_v2：MultiWOZ 2.2 test 子句，8 类单错误 + noise:multi 多错误 + 复核后的 negative:clean",
        header_common,
    ])
    write_tsv(args.out_ood, ood_rows, [
        "AnIME ood_v1：LibriSpeech LM 语料 CRC32 留出句（文学领域）；须经 build_ngram_kn.py 去泄漏后重建 L2",
        header_common,
    ])
    if tm2_rows:
        write_tsv(args.out_ood_tm2, tm2_rows, [
            "AnIME ood_tm2_v1：Taskmaster-2 (Byrne et al., Google LLC, CC BY 4.0 https://creativecommons.org/licenses/by/4.0/) USER 话轮",
            "food-ordering / music / sports / movies 领域；仅评测，L2 构建按 eval/** 去泄漏；" + header_common,
        ])
        print(f"ood_tm2_v1: {len(tm2_rows)} rows {dict(sorted(tm2_stats.items()))}")
    if dev_rows:
        write_tsv(args.out_ood_dev, dev_rows + novel_rows, [
            "AnIME ood_dev_v1：OpenAssistant oasst1（Apache-2.0）按对话树留出的英文用户提问（开发者 / 现代日常文本）",
            "negative:novel = 含词典外新词的干净句；留出树不进入 L2 训练（build_oasst_text.py）；" + header_common,
        ])
        print(f"ood_dev_v1: {len(dev_rows) + len(novel_rows)} rows {dict(sorted(dev_stats.items()))}")
    if args.emit_negative_candidates:
        cands = [("sentence", "", s, s, "negative:clean") for s in woz_cands + libri_cands + tm2_cands + dev_cands]
        cands += [(r[0], r[1], r[2], r[3], "negative:novel") for r in novel_rows]
        write_tsv(args.emit_negative_candidates, cands, ["未复核的干净反例候选（仅用于分歧裁决，勿入库）"])
    print(f"noise_v2: {len(noise_rows)} rows {dict(sorted(noise_stats.items()))}")
    print(f"ood_v1: {len(ood_rows)} rows {dict(sorted(ood_stats.items()))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
