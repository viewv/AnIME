#!/usr/bin/env python3
"""AnIME 专用纠错 L3 模型监督微调数据集构建（task-07 P1）。

每条样本：`prompt = <|ctx|> {上文}\\n<|typed|> {键入原文}\\n<|fix|>`，`target = " {目标文本}<|end|>"`，
与 `crates/lm-neural/src/prompt.rs` 的 `build_correction_prompt` / `correction_target` 逐字节一致。

- 干净文本：oasst1 训练树（剔除 ood_dev 留出树）、MultiWOZ 2.2 train / dev（LibriSpeech 默认不用：全小写会教坏专名大小写）；
  按标点切成 2–16 词子句（输入法预编辑里不含标点），上文取同一会话中的前一子句 / 父消息末尾。
- 噪声：约 35% 照抄（含词典外新词的子句一律照抄，教模型不误改新词；照抄样本一半把原文小写，
  兼学大小写恢复）、约 45% 单错误、约 20% 多错误；噪声只作用于词典词。
- 去泄漏：与 `eval/**` 全部参考文本及上文做整句 + 5-gram 过滤（归一化口径与 anime-ngram-build 一致）。
- 验证集按会话 ID 哈希切出约 5%，同一会话不跨训练 / 验证。
"""

from __future__ import annotations

import argparse
import csv
import gzip
import json
import random
import re
import sys
import zlib
from collections import Counter, defaultdict
from pathlib import Path
from typing import Iterator

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from build_eval_benchmark import ALPHA_WORD_RE  # noqa: E402
from build_eval_v2 import (  # noqa: E402
    DROPPED_APOSTROPHE_FORMS,
    SENTENCE_NOISERS,
    load_lexicon,
    load_realword_vocab,
    single_edits,
)
from build_eval_v3 import noise_multi  # noqa: E402
from build_ngram_kn import collect_eval_texts, eval_tsv_paths  # noqa: E402
from build_oasst_text import DEFAULT_IN as OASST_IN, clean_markdown, is_heldout_tree, keep_message  # noqa: E402

CTX_MARK, TYPED_MARK, FIX_MARK, END_MARK = "<|ctx|>", "<|typed|>", "<|fix|>", "<|end|>"
CORRECTION_MAX_CONTEXT_CHARS = 160
LEAK_NGRAM = 5
MIN_WORDS, MAX_WORDS = 2, 16
KNOWN_MIN_ZIPF = 2.5
VAL_PER_100 = 5

# 单错误类型分布：偏向开发者 / 口语快打中常见的邻键、空格与漏首字母
SINGLE_NOISE_WEIGHTS = {
    "adjacent_key": 0.18,
    "real_typo": 0.13,
    "leading_drop": 0.12,
    "space_shift": 0.12,
    "space_missing": 0.12,
    "space_extra": 0.10,
    "realword_edit": 0.10,
    "boundary_gemination": 0.08,
    "contraction": 0.05,
}

_SPLIT_RE = re.compile(r"[.!?,;:()\[\]{}\"]+|\s-\s")


# ---------- 与 Rust 侧逐字节一致的模板 ----------

_ASCII_WS = " \t\n\r\x0c"


def _trim_context_tail(text: str, max_chars: int) -> str:
    """逐行对齐 Rust `prompt::trim_context_tail`：取末尾 max_chars 个字符，并跳到第一个 ASCII 空白之后，
    避免从半个单词开始。"""
    if len(text) <= max_chars or max_chars == 0:
        return text
    tail = text[len(text) - max_chars:]
    ws = next((i for i, c in enumerate(tail) if c in _ASCII_WS), -1)
    if ws >= 0:
        after = tail[ws:]
        if after.strip():
            return after.lstrip(" \t")
    return tail


def build_correction_prompt(context_before: str, typed: str) -> str:
    ctx = " ".join(_trim_context_tail(context_before, CORRECTION_MAX_CONTEXT_CHARS).split())
    typed = " ".join(typed.split())
    return f"{CTX_MARK} {ctx}\n{TYPED_MARK} {typed}\n{FIX_MARK}"


def correction_target(candidate: str) -> str:
    return " " + " ".join(candidate.split()) + END_MARK


# ---------- 去泄漏（口径与 anime-ngram-build normalize_into 一致） ----------

def normalize_sentences(text: str) -> list[list[str]]:
    out: list[list[str]] = []
    sent: list[str] = []
    tok: list[str] = []

    def flush_tok() -> None:
        t = "".join(tok).strip("'")
        if t:
            sent.append(t)
        tok.clear()

    for ch in text.replace("’", "'").replace("‘", "'"):
        c = ch.lower() if ch.isascii() else ch
        if "a" <= c <= "z" or c == "'":
            tok.append(c)
        elif c in " \t-":
            flush_tok()
        else:
            flush_tok()
            if sent:
                out.append(sent)
                sent = []
    flush_tok()
    if sent:
        out.append(sent)
    return out


class LeakFilter:
    def __init__(self, texts: list[str]):
        self.sentences: set[str] = set()
        self.ngrams: set[tuple[str, ...]] = set()
        for t in texts:
            for s in normalize_sentences(t):
                self.sentences.add(" ".join(s))
                for i in range(len(s) - LEAK_NGRAM + 1):
                    self.ngrams.add(tuple(s[i:i + LEAK_NGRAM]))

    def is_leak(self, text: str) -> bool:
        for s in normalize_sentences(text):
            if " ".join(s) in self.sentences:
                return True
            for i in range(len(s) - LEAK_NGRAM + 1):
                if tuple(s[i:i + LEAK_NGRAM]) in self.ngrams:
                    return True
        return False


# ---------- 干净子句来源 ----------

def clauses(text: str) -> list[list[str]]:
    """标点切分后的 2–16 词纯字母（含撇号）子句；不满足的片段丢弃。"""
    out = []
    for seg in _SPLIT_RE.split(text):
        toks = seg.split()
        if MIN_WORDS <= len(toks) <= MAX_WORDS and all(ALPHA_WORD_RE.match(t) for t in toks):
            if not any(t.lower() in DROPPED_APOSTROPHE_FORMS for t in toks):
                out.append(toks)
    return out


def oasst_units(path: Path) -> Iterator[tuple[str, str, str, str]]:
    """(会话 ID, 来源标签, 上文, 子句文本)：上文为父消息末尾 + 同消息内前一子句。"""
    by_id: dict[str, dict] = {}
    with gzip.open(path, "rt", encoding="utf-8") as f:
        for raw in f:
            m = json.loads(raw)
            if keep_message(m) and not is_heldout_tree(m.get("message_tree_id", "")):
                by_id[m["message_id"]] = m
    for m in by_id.values():
        parent = by_id.get(m.get("parent_id") or "")
        parent_tail = " ".join(clean_markdown(parent["text"]))[-CORRECTION_MAX_CONTEXT_CHARS:] if parent else ""
        prev = parent_tail
        for para in clean_markdown(m["text"]):
            for c in clauses(para):
                text = " ".join(c)
                yield m["message_tree_id"], f"oasst_{m['role']}", prev, text
                prev = text


def multiwoz_units(paths: list[Path]) -> Iterator[tuple[str, str, str, str]]:
    for p in sorted(paths):
        for dlg in json.loads(p.read_text(encoding="utf-8")):
            prev = ""
            for turn in dlg.get("turns", []):
                utt = " ".join(str(turn.get("utterance", "")).split())
                for c in clauses(utt):
                    text = " ".join(c)
                    yield dlg.get("dialogue_id", p.name), "multiwoz", prev, text
                    prev = text


def librispeech_units(gz_path: Path, per_100k: int) -> Iterator[tuple[str, str, str, str]]:
    with gzip.open(gz_path, "rt", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            # 与 ood_v1 的留出桶不同的 CRC 段（训练只取 100000 桶中的高位段）
            if not line or zlib.crc32(line.encode()) % 100_000 < 100_000 - per_100k:
                continue
            toks = line.lower().split()
            if MIN_WORDS <= len(toks) <= MAX_WORDS:
                yield f"libri:{zlib.crc32(line.encode())}", "librispeech", "", " ".join(toks)


# ---------- 噪声 ----------

def load_real_typos(path: Path, lex) -> dict[str, list[str]]:
    """crate-ci/typos（MIT）：正确词 -> 真实错拼列表，只保留词典常见词。"""
    out: dict[str, list[str]] = defaultdict(list)
    if not path.exists():
        return out
    with path.open(encoding="utf-8", errors="replace", newline="") as fh:
        for row in csv.reader(fh):
            if len(row) < 2:
                continue
            typo = row[0].strip().lower()
            for target in row[1:]:
                target = target.strip().lower()
                hit = lex.get(target)
                if typo.isalpha() and target.isalpha() and typo != target and hit and hit[1] >= 3.0:
                    out[target].append(typo)
    return out


def noise_real_typo(toks: list[str], rng: random.Random, real_typos) -> str | None:
    idx = [i for i, w in enumerate(toks) if w.lower() in real_typos]
    if not idx:
        return None
    i = rng.choice(idx)
    out = [w.lower() for w in toks]
    out[i] = rng.choice(real_typos[toks[i].lower()])
    return " ".join(out)


def noise_realword(toks: list[str], rng: random.Random, realword_vocab: set[str]) -> str | None:
    order = list(range(len(toks)))
    rng.shuffle(order)
    for i in order:
        w = toks[i]
        if not w.isalpha() or len(w) < 2:
            continue
        cands = sorted(e for e in single_edits(w) if e in realword_vocab)
        if cands:
            out = [t.lower() for t in toks]
            out[i] = rng.choice(cands)
            return " ".join(out)
    return None


def make_noisy(toks: list[str], ntype: str, rng: random.Random, lex, realword_vocab, real_typos) -> str | None:
    if ntype == "real_typo":
        return noise_real_typo(toks, rng, real_typos)
    if ntype == "realword_edit":
        return noise_realword(toks, rng, realword_vocab)
    if ntype == "multi":
        return noise_multi(toks, rng, lex)
    return SENTENCE_NOISERS[ntype](toks, rng, lex)


def is_known(tok: str, lex) -> bool:
    if tok.lower() == "i":
        return True
    hit = lex.get(tok.lower())
    return hit is not None and hit[1] >= KNOWN_MIN_ZIPF


def make_example(ctx: str, text: str, rng: random.Random, lex, realword_vocab, real_typos,
                 identity_rate: float, multi_rate: float) -> tuple[str, str, str] | None:
    """返回 (键入原文, 目标文本, 噪声标签)；新词子句一律照抄。"""
    toks = text.split()
    has_novel = not all(is_known(t, lex) for t in toks)
    r = rng.random()
    if has_novel or r < identity_rate:
        typed = text if rng.random() < 0.5 else text.lower()
        return typed, text, "identity_novel" if has_novel else "identity"
    if r < identity_rate + multi_rate:
        ntype = "multi"
    else:
        types, weights = zip(*SINGLE_NOISE_WEIGHTS.items())
        ntype = rng.choices(types, weights=weights)[0]
    for attempt in (ntype, "adjacent_key"):
        noisy = make_noisy(toks, attempt, rng, lex, realword_vocab, real_typos)
        if noisy and noisy.lower() != text.lower() and "  " not in noisy:
            return noisy, text, attempt
    return None


# ---------- 主流程 ----------

def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--lexicon-tsv", type=Path, default=REPO_ROOT / "data/compiled/lexicon.tsv")
    ap.add_argument("--oasst", type=Path, default=OASST_IN)
    ap.add_argument("--multiwoz-glob", action="append",
                    default=["data/raw/multiwoz-2.2-train-*/dialogues_*.json", "data/raw/multiwoz-2.2-dev-*/dialogues_*.json"])
    ap.add_argument("--librispeech", type=Path, default=REPO_ROOT / "data/raw/ngram-corpus/librispeech-lm-norm.txt.gz")
    ap.add_argument("--librispeech-per-100k", type=int, default=0,
                    help="LibriSpeech 抽样比例（每 10 万行）；默认 0：其文本全小写无标点，会教模型把专名改成小写")
    ap.add_argument("--typos-csv", type=Path, default=REPO_ROOT / "data/raw/crate-ci-typos/words.csv")
    ap.add_argument("--out-dir", type=Path, default=REPO_ROOT / "data/compiled/l3_sft")
    ap.add_argument("--identity-rate", type=float, default=0.35)
    ap.add_argument("--multi-rate", type=float, default=0.20)
    ap.add_argument("--variants", default="oasst_prompter=6,multiwoz=2,oasst_assistant=1,librispeech=1",
                    help="各来源每个干净子句生成的样本数：用户提问最接近真实键入，加权；志愿者长回答文风正式，降权")
    ap.add_argument("--max-per-source", type=int, default=0, help="每个来源最多子句数（0 = 不限，调试用）")
    ap.add_argument("--seed", type=int, default=20261123)
    args = ap.parse_args(argv)

    variants = {k: int(v) for k, v in (kv.split("=") for kv in args.variants.split(","))}
    rng = random.Random(args.seed)
    lex = load_lexicon(args.lexicon_tsv)
    realword_vocab = load_realword_vocab(args.lexicon_tsv)
    real_typos = load_real_typos(args.typos_csv, lex)
    leak = LeakFilter(collect_eval_texts(eval_tsv_paths(REPO_ROOT)))
    print(f"eval leak reference: {len(leak.sentences)} sentences, {len(leak.ngrams)} 5-grams; real typos for {len(real_typos)} words", flush=True)

    sources = []
    if args.oasst.exists():
        sources.append(oasst_units(args.oasst))
    woz = sorted({p for g in args.multiwoz_glob for p in REPO_ROOT.glob(g)})
    if any("-test-" in str(p) for p in woz):
        print("refusing MultiWOZ test shards (eval source)", file=sys.stderr)
        return 1
    if woz:
        sources.append(multiwoz_units(woz))
    if args.librispeech.exists() and args.librispeech_per_100k > 0:
        sources.append(librispeech_units(args.librispeech, args.librispeech_per_100k))

    args.out_dir.mkdir(parents=True, exist_ok=True)
    stats: Counter = Counter()
    seen: set[tuple[str, str]] = set()
    per_source: Counter = Counter()
    with (args.out_dir / "train.jsonl").open("w", encoding="utf-8") as ftr, \
         (args.out_dir / "val.jsonl").open("w", encoding="utf-8") as fva:
        for units in sources:
            for conv_id, src, ctx, text in units:
                if args.max_per_source and per_source[src] >= args.max_per_source:
                    continue
                if leak.is_leak(text) or (ctx and leak.is_leak(ctx)):
                    stats["leak_dropped"] += 1
                    continue
                per_source[src] += 1
                split = "val" if zlib.crc32(str(conv_id).encode()) % 100 < VAL_PER_100 else "train"
                # 约 30% 样本不带上文，覆盖输入框起始与前端拿不到 surrounding text 的情况
                use_ctx = ctx if rng.random() >= 0.30 else ""
                for _ in range(variants.get(src, 1)):
                    ex = make_example(use_ctx, text, rng, lex, realword_vocab, real_typos,
                                      args.identity_rate, args.multi_rate)
                    if ex is None:
                        stats["noise_failed"] += 1
                        continue
                    typed, target, tag = ex
                    if (typed, use_ctx) in seen:
                        continue
                    seen.add((typed, use_ctx))
                    rec = {
                        "prompt": build_correction_prompt(use_ctx, typed),
                        "target": correction_target(target),
                        "meta": {"src": src, "noise": tag},
                    }
                    (fva if split == "val" else ftr).write(json.dumps(rec, ensure_ascii=False) + "\n")
                    stats[f"{split}_examples"] += 1
                    stats[f"noise:{tag}"] += 1
                    stats[f"src:{src}"] += 1
                    if tag.startswith("identity"):
                        break  # 照抄样本每个子句只留一条，避免重复
    stats.update({f"clauses:{k}": v for k, v in per_source.items()})
    (args.out_dir / "stats.json").write_text(json.dumps(dict(sorted(stats.items())), indent=2) + "\n", encoding="utf-8")
    print(json.dumps(dict(sorted(stats.items())), indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
