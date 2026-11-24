#!/usr/bin/env python3
"""AnIME 单词模式上下文评测集 word_ctx_v1：模拟 `space_commits = true` 用户逐词上屏。

每行只敲**一个词**，`context` 是同一子句中该词之前的干净词（假定前面的词都已正确上屏），
`expected` 是原句中的该词。用于衡量单词模式的单 token 排序（L1 召回 + L2 单 token 上下文加分）。

- 源句只取评测留出数据，均不进入 L2 训练：
  - OpenAssistant oasst1 按对话树留出的用户提问（`build_oasst_text.is_heldout_tree`，Apache-2.0）；
  - arXiv CS 按论文 ID 留出的摘要（`data/compiled/arxiv_cs/heldout.jsonl`，元数据 CC0）。
- 词级噪声（`category` 即错误类别，`notes` 记录算子与位置）：
  - `prefix`：前缀截断（尚未打完，截断前缀本身不是词典词）；
  - `typo_simple`：单次 QWERTY 邻键替换 / 漏字 / 邻键多打 / 相邻换位，漏字位置覆盖词首附近（第 2–3 个字母）、
    词中与词尾；
  - `typo_prefix`：词前部一处错误后再截断；
  - `typo_phonetic`：读音式拼写（ph->f、双写辅音单写、ie/ei、非重读元音互换、tion->shun 等）叠加至少两处；
  - `context_realword`：打成另一个常见真词（单次编辑），需靠上文消歧；
  - `exact` + `negative:clean`：照敲的常见词，监控误纠；
  - `exact` + `negative:novel`：词典外新词 / 技术词（`PyTorch`、`kubectl`），照敲，监控"不误改新词"。
- `notes` 首个 token 为 `source:oasst` / `source:arxiv`（反例为 `negative:clean` / `negative:novel`），
  其后 `op=<算子> pos=<early|mid|late>` 与 `原词->输入` 供失误分析，不参与匹配。
- 反例复核沿用 `eval/review/negative_review.tsv`：被引擎改动的干净反例逐条人工裁决，源句自身有错的
  子句（`exclude`）不进入任何行。`--emit-negative-candidates` 导出未复核的反例候选。

全部抽样由 `(seed, 类别, 子句)` 的 CRC32 稳定排序与按子句派生的随机源决定：复核剔除一句只会顺延下一句。
"""

from __future__ import annotations

import argparse
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

from build_eval_benchmark import QWERTY_NEIGHBORS, is_valid_sample  # noqa: E402
from build_eval_benchmark import damerau_levenshtein  # noqa: E402
from build_eval_v2 import inflection_variants, load_lexicon, load_realword_vocab, single_edits, write_tsv  # noqa: E402
from build_eval_v3 import arxiv_heldout_abstracts, dev_clauses, load_review  # noqa: E402
from build_oasst_text import DEFAULT_IN as OASST_IN, heldout_prompter_paragraphs  # noqa: E402

Row = tuple[str, str, str, str, str]
VOWELS = "aeiou"
WORD_RE = re.compile(r"^[a-z]+$")
# 噪声目标词必须是常见词：罕见词的"正确答案"本身就不稳定，且不代表用户日常输入
MIN_TARGET_ZIPF = 3.0
MIN_NEGATIVE_ZIPF = 2.5
# 读音式拼写算子：(正确拼写片段, 读音相近的错拼片段)；以 `$` 结尾的只作用于词尾（后缀混淆）
PHONETIC_SUBS = (
    ("ph", "f"), ("ck", "k"), ("que$", "k"), ("tion", "shun"), ("sion", "tion"), ("cial", "shal"),
    ("ous$", "us"), ("ence$", "ance"), ("ance$", "ence"), ("ible$", "able"), ("able$", "ible"),
    ("ie", "ei"), ("ei", "ie"), ("ce$", "se"), ("cy$", "sy"), ("ght", "t"), ("kn", "n"), ("wr", "r"),
    ("ture$", "cher"), ("qu", "kw"), ("ea", "ee"), ("er$", "or"), ("or$", "er"), ("ary$", "ery"),
    ("ery$", "ary"), ("al$", "el"), ("ent$", "ant"), ("ant$", "ent"), ("sc", "s"), ("wh", "w"),
)
# 非重读元音（schwa）在拼写里最常互换的是 a / e / i（seperate、definate、independant）
SCHWA_VOWELS = "aei"


def pos_bucket(p: int, n: int) -> str:
    if p <= 2:
        return "early"
    if p >= n - 2:
        return "late"
    return "mid"


def op_omit(w: str, rng: random.Random, early: bool) -> tuple[str, str] | None:
    # 漏首字母极少见且无法纠回，从第 2 个字母起；early 专测词首附近（第 2–3 个字母）漏字
    lo, hi = (1, min(2, len(w) - 2)) if early else (1, len(w) - 1)
    if hi < lo:
        return None
    p = rng.randint(lo, hi)
    if w[p] == w[p - 1]:
        return None  # 双写字母漏一个属于读音类
    return w[:p] + w[p + 1 :], f"op=omit pos={pos_bucket(p, len(w))}"


def op_adjacent(w: str, rng: random.Random) -> tuple[str, str] | None:
    p = rng.randrange(len(w))
    nb = QWERTY_NEIGHBORS.get(w[p], "")
    if not nb:
        return None
    return w[:p] + rng.choice(nb) + w[p + 1 :], f"op=adjacent pos={pos_bucket(p, len(w))}"


def op_insert(w: str, rng: random.Random) -> tuple[str, str] | None:
    p = rng.randrange(len(w))
    nb = QWERTY_NEIGHBORS.get(w[p], "")
    if not nb:
        return None
    q = p + rng.randint(0, 1)
    return w[:q] + rng.choice(nb) + w[q:], f"op=insert pos={pos_bucket(p, len(w))}"


def op_transpose(w: str, rng: random.Random) -> tuple[str, str] | None:
    p = rng.randrange(len(w) - 1)
    if w[p] == w[p + 1]:
        return None
    return w[:p] + w[p + 1] + w[p] + w[p + 2 :], f"op=transpose pos={pos_bucket(p, len(w))}"


SIMPLE_OPS = (
    ("omit_early", lambda w, r: op_omit(w, r, True)),
    ("omit", lambda w, r: op_omit(w, r, False)),
    ("adjacent", op_adjacent),
    ("insert", op_insert),
    ("transpose", op_transpose),
)


def phonetic_edits(w: str) -> list[str]:
    """结构化读音式改写（每个返回一个新串）：片段替换、双写辅音单写、词尾不发音 e。"""
    out: list[str] = []
    for src, dst in PHONETIC_SUBS:
        if src.endswith("$"):
            src = src[:-1]
            i = len(w) - len(src)
            if i > 0 and w.endswith(src):
                out.append(w[:i] + dst)
            continue
        i = w.find(src, 1)
        if i > 0:
            out.append(w[:i] + dst + w[i + len(src) :])
    for i in range(1, len(w) - 1):
        if w[i] == w[i + 1] and w[i] not in VOWELS:
            out.append(w[:i] + w[i + 1 :])
    if len(w) >= 5 and w.endswith("e") and w[-2] not in VOWELS:
        out.append(w[:-1])
    return [x for x in out if x and x != w]


def vowel_swap(w: str, rng: random.Random) -> str | None:
    # 非重读元音互换（seperate / definately）：跳过第一个元音（多为重读音节），只在 a / e / i 之间互换，
    # 且前后不能紧邻其他元音（双元音拼写不属于 schwa）
    vpos = [i for i in range(1, len(w) - 1) if w[i] in VOWELS]
    vpos = [
        i for i in vpos[1:]
        if w[i] in SCHWA_VOWELS and w[i - 1] not in VOWELS and w[i + 1] not in VOWELS
    ]
    if not vpos:
        return None
    i = rng.choice(vpos)
    return w[:i] + rng.choice([v for v in SCHWA_VOWELS if v != w[i]]) + w[i + 1 :]


def phonetic_noise(w: str, rng: random.Random) -> tuple[str, str] | None:
    """至少一处结构化读音改写，再叠加一处结构化改写或一次元音互换；与原词编辑距离 >= 2。"""
    first = phonetic_edits(w)
    if not first:
        return None
    cur = rng.choice(first)
    second = phonetic_edits(cur)
    if second and rng.random() < 0.5:
        cur, kind = rng.choice(second), "struct2"
    else:
        swapped = vowel_swap(cur, rng)
        if swapped is None:
            return None
        cur, kind = swapped, "struct+vowel"
    if damerau_levenshtein(cur, w) < 2:
        return None
    return cur, f"op=phonetic kind={kind}"


class Builder:
    def __init__(self, lex, realword_vocab: set[str], excluded: set[str], seed: int):
        self.lex = lex
        self.realword = realword_vocab
        self.excluded = excluded
        self.seed = seed
        self.seen: set[tuple[str, str, str]] = set()
        self.stats: Counter = Counter()

    def zipf(self, w: str) -> float:
        hit = self.lex.get(w.lower())
        return hit[1] if hit else 0.0

    def is_word(self, s: str) -> bool:
        return s.lower() in self.lex

    def is_prefix_of_common(self, s: str, target: str) -> bool:
        return target.lower().startswith(s)

    def rng_for(self, tag: str, clause: list[str]) -> random.Random:
        return random.Random(zlib.crc32(f"{self.seed}:{tag}:{' '.join(clause).lower()}".encode()))

    def ordered(self, clauses: list[list[str]], tag: str) -> list[list[str]]:
        usable = [c for c in clauses if " ".join(c).lower() not in self.excluded]
        return sorted(usable, key=lambda c: zlib.crc32(f"{self.seed}:{tag}:{' '.join(c).lower()}".encode()))

    def emit(self, rows: list[Row], cat: str, toks: list[str], i: int, inp: str, exp: str, note: str,
             novel: bool = False) -> bool:
        ctx = " ".join(toks[:i]) + (" " if i > 0 else "")
        if not novel and not is_valid_sample(cat, ctx, inp, exp.split(" || ")[0]):
            return False
        key = (cat, ctx, inp)
        if key in self.seen:
            return False
        self.seen.add(key)
        rows.append((cat, ctx, inp, exp, note))
        return True

    def positions(self, toks: list[str], rng: random.Random, min_len: int, min_zipf: float) -> list[int]:
        idx = [
            i for i, t in enumerate(toks)
            if WORD_RE.match(t) and len(t) >= min_len and self.zipf(t) >= min_zipf
        ]
        rng.shuffle(idx)
        return idx

    def take(self, rows: list[Row], clauses, src: str, tag: str, quota: int, make) -> None:
        n = 0
        for c in self.ordered(clauses, f"{src}:{tag}"):
            if n >= quota:
                break
            rng = self.rng_for(f"{src}:{tag}", c)
            # 每个子句至多贡献一行，避免同一上文反复出现
            if make(rows, c, rng, src):
                n += 1
        self.stats[f"{src}:{tag}"] = n

    # ---- 各类别 ----
    def make_prefix(self, rows, toks, rng, src) -> bool:
        for i in self.positions(toks, rng, 6, MIN_TARGET_ZIPF):
            w = toks[i]
            k = rng.randint(max(4, (len(w) + 1) // 2), len(w) - 1)
            pre = w[:k]
            if self.is_word(pre):
                continue
            # 截断前缀在屈折变体间真正同等合理（`worke` -> worker / workers），按契约以 `||` 列出
            variants = [v for v in inflection_variants(w, self.lex) if v.lower().startswith(pre) and len(v) > len(pre)]
            exp = " || ".join([w] + variants)
            if self.emit(rows, "prefix", toks, i, pre, exp, f"source:{src} op=prefix keep={k}/{len(w)} {w}->{pre}"):
                return True
        return False

    def make_simple(self, op_name, op):
        def make(rows, toks, rng, src) -> bool:
            for i in self.positions(toks, rng, 4 if op_name != "omit_early" else 5, MIN_TARGET_ZIPF):
                w = toks[i]
                res = op(w, rng)
                if res is None:
                    continue
                noisy, note = res
                if self.is_word(noisy) or noisy == w:
                    continue
                if self.emit(rows, "typo_simple", toks, i, noisy, w, f"source:{src} {note} {w}->{noisy}"):
                    return True
            return False
        return make

    def make_typo_prefix(self, rows, toks, rng, src) -> bool:
        for i in self.positions(toks, rng, 7, MIN_TARGET_ZIPF):
            w = toks[i]
            k = rng.randint(max(4, (len(w) + 1) // 2), len(w) - 1)
            head = w[:k]
            op_name, op = rng.choice(SIMPLE_OPS)
            res = op(head, rng) if len(head) >= 4 else None
            if res is None:
                continue
            noisy, note = res
            if noisy == head or w.startswith(noisy) or self.is_word(noisy):
                continue
            variants = [v for v in inflection_variants(w, self.lex) if v.lower().startswith(head)]
            exp = " || ".join([w] + variants)
            if self.emit(rows, "typo_prefix", toks, i, noisy, exp,
                         f"source:{src} {note} keep={k}/{len(w)} {w}->{noisy}"):
                return True
        return False

    def make_phonetic(self, rows, toks, rng, src) -> bool:
        for i in self.positions(toks, rng, 5, MIN_TARGET_ZIPF):
            w = toks[i]
            res = phonetic_noise(w, rng)
            if res is None:
                continue
            noisy, note = res
            if self.is_word(noisy) or not noisy.isalpha() or len(noisy) < 3:
                continue
            if self.emit(rows, "typo_phonetic", toks, i, noisy, w, f"source:{src} {note} {w}->{noisy}"):
                return True
        return False

    def make_realword(self, rows, toks, rng, src) -> bool:
        for i in self.positions(toks, rng, 2, MIN_TARGET_ZIPF):
            if i == 0:
                continue  # 真词混淆需要上文
            w = toks[i]
            cands = sorted(e for e in single_edits(w) if e in self.realword and e != w.lower())
            if not cands:
                continue
            wrong = rng.choice(cands)
            if self.emit(rows, "context_realword", toks, i, wrong, w, f"source:{src} op=realword {w}->{wrong}"):
                return True
        return False

    def make_negative(self, cands: list[Row]):
        def make(rows, toks, rng, src) -> bool:
            for i in self.positions(toks, rng, 3, MIN_NEGATIVE_ZIPF):
                w = toks[i]
                if w != w.lower():
                    continue
                note = f"negative:clean source={src}"
                if self.emit(rows, "exact", toks, i, w, w, note):
                    cands.append(rows[-1])
                    return True
            return False
        return make

    def make_novel(self, rows, toks, rng, src) -> bool:
        idx = [
            i for i, t in enumerate(toks)
            if (t.lower() not in self.lex or self.zipf(t) < 2.5) and t.isalpha() and len(t) >= 3 and t.lower() != "i"
        ]
        rng.shuffle(idx)
        for i in idx:
            w = toks[i]
            if self.emit(rows, "exact", toks, i, w, w, f"negative:novel source={src}", novel=True):
                return True
        return False


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--lexicon-tsv", type=Path, default=REPO_ROOT / "data/compiled/lexicon.tsv")
    ap.add_argument("--oasst", type=Path, default=OASST_IN)
    ap.add_argument("--arxiv-heldout", type=Path, default=REPO_ROOT / "data/compiled/arxiv_cs/heldout.jsonl")
    ap.add_argument("--review-file", type=Path, default=REPO_ROOT / "eval/review/negative_review.tsv")
    ap.add_argument("--out", type=Path, default=REPO_ROOT / "eval/benchmarks/word_ctx_v1.tsv")
    ap.add_argument("--emit-negative-candidates", type=Path, default=None)
    ap.add_argument("--per-category", type=int, default=300, help="每个噪声类别的总行数（两来源合计）")
    ap.add_argument("--simple-per-op", type=int, default=100, help="typo_simple 每个算子的总行数")
    ap.add_argument("--negatives", type=int, default=400)
    ap.add_argument("--novel", type=int, default=200)
    ap.add_argument("--arxiv-share", type=float, default=0.4)
    ap.add_argument("--seed", type=int, default=20261124)
    args = ap.parse_args(argv)

    lex = load_lexicon(args.lexicon_tsv)
    realword_vocab = load_realword_vocab(args.lexicon_tsv)
    excluded = load_review(args.review_file)
    dev_std, dev_novel = dev_clauses(heldout_prompter_paragraphs(args.oasst), lex)
    acad_std, acad_novel = dev_clauses(arxiv_heldout_abstracts(args.arxiv_heldout), lex, keep_source_case=True)
    print(f"clauses: oasst={len(dev_std)}+{len(dev_novel)}novel arxiv={len(acad_std)}+{len(acad_novel)}novel")

    b = Builder(lex, realword_vocab, excluded, args.seed)
    rows: list[Row] = []
    neg_cands: list[Row] = []

    def split(total: int) -> tuple[int, int]:
        a = round(total * args.arxiv_share)
        return total - a, a

    sources = (("oasst", dev_std, dev_novel), ("arxiv", acad_std, acad_novel))
    jobs = [
        ("prefix", args.per_category, b.make_prefix),
        *[(f"typo_simple_{name}", args.simple_per_op, b.make_simple(name, op)) for name, op in SIMPLE_OPS],
        ("typo_prefix", args.per_category, b.make_typo_prefix),
        ("typo_phonetic", args.per_category, b.make_phonetic),
        ("context_realword", args.per_category, b.make_realword),
        ("negative_clean", args.negatives, b.make_negative(neg_cands)),
    ]
    for tag, total, make in jobs:
        for (src, std, _), quota in zip(sources, split(total)):
            b.take(rows, std, src, tag, quota, make)
    for (src, _, novel), quota in zip(sources, split(args.novel)):
        b.take(rows, novel, src, "negative_novel", quota, b.make_novel)

    write_tsv(args.out, rows, [
        "AnIME word_ctx_v1：单词模式逐词上屏评测（每行一个词，context = 同子句中其前的干净词）",
        "源句：oasst1 留出树用户提问（Apache-2.0）+ arXiv CS 留出摘要（元数据 CC0），均不进入 L2 训练；"
        "推荐 anime-eval --mode word --group-by-tag",
        f"seed={args.seed}；由 scripts/data_pipeline/build_word_ctx_eval.py 生成（勿手改），反例经 eval/review/negative_review.tsv 复核",
    ])
    if args.emit_negative_candidates:
        write_tsv(args.emit_negative_candidates, neg_cands, ["未复核的单词反例候选（仅用于分歧裁决，勿入库）"])
    cats = Counter(r[0] for r in rows)
    print(f"word_ctx_v1: {len(rows)} rows {dict(sorted(cats.items()))}")
    print(json.dumps(dict(sorted(b.stats.items())), ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
