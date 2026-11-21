#!/usr/bin/env python3
"""AnIME 2/3-gram 统计语言模型构建脚本：流式处理开源英文语料（.txt/.txt.gz）或 ARPA（.arpa/.arpa.gz），输出 ANIMENGR v1。"""

from __future__ import annotations

import argparse
import gzip
import json
import math
import re
import sys
from collections import Counter
from pathlib import Path
from typing import TextIO

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from common import write_animengr  # noqa: E402

WORD_TOKEN_RE = re.compile(r"[A-Za-z]+(?:'[A-Za-z]+)?")
SENTENCE_SPLIT_RE = re.compile(r"[.!?\n]+")

SPECIAL_UNK = "<unk>"
SPECIAL_BOS = "<bos>"
SPECIAL_EOS = "<eos>"


def open_text_maybe_gz(path: Path) -> TextIO:
    if path.suffix == ".gz":
        return gzip.open(path, "rt", encoding="utf-8", errors="replace")
    return path.open("r", encoding="utf-8", errors="replace")


def tokenize_sentences(line: str) -> list[list[str]]:
    sentences: list[list[str]] = []
    for seg in SENTENCE_SPLIT_RE.split(line):
        tokens = [m.group(0).lower() for m in WORD_TOKEN_RE.finditer(seg)]
        if tokens:
            sentences.append(tokens)
    return sentences


def build_from_corpus(
    corpus_paths: list[Path],
    max_lines: int,
    min_unigram: int,
    min_bigram: int,
    min_trigram: int,
    max_vocab: int,
    alpha: float,
) -> tuple[
    list[str],
    list[tuple[int, float, float]],
    list[tuple[int, int, float, float]],
    list[tuple[int, int, int, float]],
    dict[str, int],
]:
    uni_counts: Counter[str] = Counter()
    bi_counts: Counter[tuple[str, str]] = Counter()
    tri_counts: Counter[tuple[str, str, str]] = Counter()

    lines_processed = 0
    sentences_processed = 0
    total_word_tokens = 0

    for path in corpus_paths:
        with open_text_maybe_gz(path) as fh:
            for raw in fh:
                if max_lines > 0 and lines_processed >= max_lines:
                    break
                lines_processed += 1
                for sent in tokenize_sentences(raw):
                    sentences_processed += 1
                    total_word_tokens += len(sent)
                    seq = [SPECIAL_BOS, *sent, SPECIAL_EOS]
                    for w in seq:
                        uni_counts[w] += 1
                    for i in range(len(seq) - 1):
                        bi_counts[(seq[i], seq[i + 1])] += 1
                    for i in range(len(seq) - 2):
                        tri_counts[(seq[i], seq[i + 1], seq[i + 2])] += 1

    # 选出高频普通词汇表（排除 3 个特殊标记后，按频次降序截断再按字典序稳定编号）
    regular = [
        (w, c)
        for w, c in uni_counts.items()
        if w not in (SPECIAL_UNK, SPECIAL_BOS, SPECIAL_EOS) and c >= min_unigram
    ]
    regular.sort(key=lambda item: (-item[1], item[0]))
    if max_vocab > 3:
        regular = regular[: max_vocab - 3]
    elif max_vocab > 0:
        regular = []

    vocab = [SPECIAL_UNK, SPECIAL_BOS, SPECIAL_EOS] + sorted(w for w, _ in regular)
    word_to_id = {w: idx for idx, w in enumerate(vocab)}

    unk_count = sum(
        c
        for w, c in uni_counts.items()
        if w not in word_to_id and w not in (SPECIAL_BOS, SPECIAL_EOS)
    )
    effective_counts: dict[int, int] = {
        word_to_id[SPECIAL_UNK]: max(1, unk_count),
        word_to_id[SPECIAL_BOS]: uni_counts.get(SPECIAL_BOS, 1),
        word_to_id[SPECIAL_EOS]: uni_counts.get(SPECIAL_EOS, 1),
    }
    for w, c in regular:
        effective_counts[word_to_id[w]] = c

    denom = float(sum(effective_counts.values())) + alpha * float(len(vocab))
    default_backoff = round(math.log10(0.4), 6)

    unigrams: list[tuple[int, float, float]] = []
    for wid, cnt in effective_counts.items():
        log10_p = math.log10((float(cnt) + alpha) / denom)
        unigrams.append((wid, log10_p, default_backoff))

    bigrams: list[tuple[int, int, float, float]] = []
    for (w1, w2), cnt in bi_counts.items():
        if cnt < min_bigram:
            continue
        id1 = word_to_id.get(w1)
        id2 = word_to_id.get(w2)
        if id1 is None or id2 is None:
            continue
        c1 = effective_counts.get(id1, 1)
        log10_p = math.log10(min(1.0, float(cnt) / float(max(1, c1))))
        bigrams.append((id1, id2, log10_p, default_backoff))

    trigrams: list[tuple[int, int, int, float]] = []
    for (w1, w2, w3), cnt in tri_counts.items():
        if cnt < min_trigram:
            continue
        id1 = word_to_id.get(w1)
        id2 = word_to_id.get(w2)
        id3 = word_to_id.get(w3)
        if id1 is None or id2 is None or id3 is None:
            continue
        c12 = max(cnt, bi_counts.get((w1, w2), cnt))
        log10_p = math.log10(min(1.0, float(cnt) / float(max(1, c12))))
        trigrams.append((id1, id2, id3, log10_p))

    stats = {
        "lines_processed": lines_processed,
        "sentences_processed": sentences_processed,
        "total_word_tokens": total_word_tokens,
        "vocab_count": len(vocab),
        "unigram_count": len(unigrams),
        "bigram_count": len(bigrams),
        "trigram_count": len(trigrams),
    }
    return vocab, unigrams, bigrams, trigrams, stats


def _norm_arpa_token(tok: str) -> str:
    low = tok.lower()
    if low == "<s>":
        return SPECIAL_BOS
    if low == "</s>":
        return SPECIAL_EOS
    if low == "<unk>":
        return SPECIAL_UNK
    return low


def build_from_arpa(
    arpa_path: Path,
    max_vocab: int,
) -> tuple[
    list[str],
    list[tuple[int, float, float]],
    list[tuple[int, int, float, float]],
    list[tuple[int, int, int, float]],
    dict[str, int],
]:
    uni_map: dict[str, tuple[float, float]] = {}
    bi_map: dict[tuple[str, str], tuple[float, float]] = {}
    tri_map: dict[tuple[str, str, str], float] = {}

    section = 0
    with open_text_maybe_gz(arpa_path) as fh:
        for raw in fh:
            line = raw.strip()
            if not line or line == "\\data\\" or line.startswith("ngram "):
                continue
            if line == "\\1-grams:":
                section = 1
                continue
            if line == "\\2-grams:":
                section = 2
                continue
            if line == "\\3-grams:":
                section = 3
                continue
            if line == "\\end\\":
                break

            parts = line.split("\t")
            if len(parts) < 2:
                continue
            log_p = float(parts[0])
            toks = [_norm_arpa_token(t) for t in parts[1].split()]
            bo = float(parts[2]) if len(parts) >= 3 else 0.0

            if section == 1 and len(toks) == 1:
                w = toks[0]
                if w not in uni_map or log_p > uni_map[w][0]:
                    uni_map[w] = (log_p, bo)
            elif section == 2 and len(toks) == 2:
                key2 = (toks[0], toks[1])
                if key2 not in bi_map or log_p > bi_map[key2][0]:
                    bi_map[key2] = (log_p, bo)
            elif section == 3 and len(toks) == 3:
                key3 = (toks[0], toks[1], toks[2])
                if key3 not in tri_map or log_p > tri_map[key3]:
                    tri_map[key3] = log_p

    regular = [
        (w, vals[0])
        for w, vals in uni_map.items()
        if w not in (SPECIAL_UNK, SPECIAL_BOS, SPECIAL_EOS)
    ]
    regular.sort(key=lambda item: (-item[1], item[0]))
    if max_vocab > 3:
        regular = regular[: max_vocab - 3]
    elif max_vocab > 0:
        regular = []

    vocab = [SPECIAL_UNK, SPECIAL_BOS, SPECIAL_EOS] + sorted(w for w, _ in regular)
    word_to_id = {w: idx for idx, w in enumerate(vocab)}

    unigrams: list[tuple[int, float, float]] = []
    for wid, w in enumerate(vocab):
        lp, bo = uni_map.get(w, (-6.0, -0.39794))
        unigrams.append((wid, lp, bo))

    bigrams: list[tuple[int, int, float, float]] = []
    for (w1, w2), (lp, bo) in bi_map.items():
        id1 = word_to_id.get(w1)
        id2 = word_to_id.get(w2)
        if id1 is not None and id2 is not None:
            bigrams.append((id1, id2, lp, bo))

    trigrams: list[tuple[int, int, int, float]] = []
    for (w1, w2, w3), lp in tri_map.items():
        id1 = word_to_id.get(w1)
        id2 = word_to_id.get(w2)
        id3 = word_to_id.get(w3)
        if id1 is not None and id2 is not None and id3 is not None:
            trigrams.append((id1, id2, id3, lp))

    stats = {
        "lines_processed": 0,
        "sentences_processed": 0,
        "total_word_tokens": 0,
        "vocab_count": len(vocab),
        "unigram_count": len(unigrams),
        "bigram_count": len(bigrams),
        "trigram_count": len(trigrams),
    }
    return vocab, unigrams, bigrams, trigrams, stats


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="从开源英文文本语料或 ARPA 语言模型构建 ANIMENGR v1 二进制 2/3-gram 表。"
    )
    parser.add_argument(
        "--corpus",
        action="append",
        type=Path,
        default=[],
        help="纯文本或 .txt.gz 语料路径（支持多个）",
    )
    parser.add_argument(
        "--arpa",
        type=Path,
        default=None,
        help="可选 ARPA / .arpa.gz 语言模型路径（如 OpenSLR 11 3-gram）",
    )
    parser.add_argument(
        "--max-lines",
        type=int,
        default=0,
        help="流式处理最大行数（0 表示全量）",
    )
    parser.add_argument("--min-unigram", type=int, default=2, help="最小 unigram 频次阈值（默认 2）")
    parser.add_argument("--min-bigram", type=int, default=2, help="最小 bigram 频次阈值（默认 2）")
    parser.add_argument("--min-trigram", type=int, default=3, help="最小 trigram 频次阈值（默认 3）")
    parser.add_argument("--max-vocab", type=int, default=65535, help="词表上限（默认 65535）")
    parser.add_argument("--alpha", type=float, default=0.1, help="Additive 平滑系数（默认 0.1）")
    parser.add_argument(
        "--out-bin",
        type=Path,
        default=REPO_ROOT / "data" / "compiled" / "ngram.animengr.bin",
        help="输出 ANIMENGR 二进制路径（默认 data/compiled/ngram.animengr.bin）",
    )
    parser.add_argument(
        "--stats-json",
        type=Path,
        default=None,
        help="可选统计摘要 JSON 输出路径",
    )
    args = parser.parse_args(argv)

    if bool(args.corpus) == (args.arpa is not None):
        parser.error("必须且仅能指定 --corpus 或 --arpa 之一")

    if args.arpa is not None:
        vocab, unigrams, bigrams, trigrams, stats = build_from_arpa(
            arpa_path=args.arpa,
            max_vocab=args.max_vocab,
        )
    else:
        vocab, unigrams, bigrams, trigrams, stats = build_from_corpus(
            corpus_paths=args.corpus,
            max_lines=args.max_lines,
            min_unigram=args.min_unigram,
            min_bigram=args.min_bigram,
            min_trigram=args.min_trigram,
            max_vocab=args.max_vocab,
            alpha=args.alpha,
        )

    write_animengr(args.out_bin, vocab, unigrams, bigrams, trigrams, flags=0)
    if args.stats_json is not None:
        args.stats_json.parent.mkdir(parents=True, exist_ok=True)
        args.stats_json.write_text(json.dumps(stats, indent=2), encoding="utf-8")

    print(
        f"ngram build complete: vocab={stats['vocab_count']} "
        f"1g={stats['unigram_count']} 2g={stats['bigram_count']} 3g={stats['trigram_count']} -> {args.out_bin}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
