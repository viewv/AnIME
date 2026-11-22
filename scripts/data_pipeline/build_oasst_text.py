#!/usr/bin/env python3
"""OpenAssistant oasst1 消息 → L2 训练用纯文本（英文、人工审核通过、按角色分文件）。

- 只保留 `lang == "en"`、`deleted == false`、`synthetic == false` 且 `review_result` 为真的消息；
- 去除 Markdown 代码块、内联代码、URL、表格 / 标题 / 列表 / 强调等标记，代码与链接在输入法里不会被逐词键入；
- `prompter`（用户提问）与 `assistant`（志愿者撰写的回答）分别写出：前者最接近真实键入文本，
  构建 L2 时可独立加权。每行一个段落，交给 `anime-ngram-build` 再做句子级归一化与去泄漏。
"""

from __future__ import annotations

import argparse
import gzip
import json
import re
import sys
import zlib
from collections import Counter
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
DEFAULT_IN = REPO_ROOT / "data/raw/oasst1/2023-04-12_oasst_ready.messages.jsonl.gz"

# 评测留出：按对话树 CRC32 抽取约 15% 的树专供 eval/benchmarks/ood_dev_v1.tsv。按树而非按消息划分，
# 保证同一段对话不会一半进训练、一半进评测。
HOLDOUT_PER_1000 = 150


def is_heldout_tree(tree_id: str) -> bool:
    tree_id = str(tree_id or "")
    return bool(tree_id) and zlib.crc32(tree_id.encode()) % 1000 < HOLDOUT_PER_1000


FENCE_RE = re.compile(r"```.*?(```|\Z)", re.S)
INLINE_CODE_RE = re.compile(r"`[^`\n]*`")
URL_RE = re.compile(r"https?://\S+|www\.\S+")
MD_LINK_RE = re.compile(r"\[([^\]]*)\]\([^)]*\)")
TABLE_LINE_RE = re.compile(r"^\s*\|.*\|\s*$")
LEAD_MARK_RE = re.compile(r"^\s*(#{1,6}\s+|[-*+]\s+|\d+[.)]\s+|>\s*)")
EMPH_RE = re.compile(r"(\*\*|__|\*|_|~~)")


def clean_markdown(text: str) -> list[str]:
    """去除代码与 Markdown 语法，返回非空段落行。"""
    t = FENCE_RE.sub("\n", text)
    t = INLINE_CODE_RE.sub(" ", t)
    t = MD_LINK_RE.sub(r"\1", t)
    t = URL_RE.sub(" ", t)
    out: list[str] = []
    for line in t.splitlines():
        if TABLE_LINE_RE.match(line):
            continue
        line = LEAD_MARK_RE.sub("", line)
        line = EMPH_RE.sub("", line)
        line = " ".join(line.split())
        if line:
            out.append(line)
    return out


def keep_message(m: dict) -> bool:
    return (
        m.get("lang") == "en"
        and not m.get("deleted")
        and not m.get("synthetic")
        and bool(m.get("review_result"))
        and m.get("role") in ("prompter", "assistant")
    )


def convert(lines, out_dir: Path) -> Counter:
    out_dir.mkdir(parents=True, exist_ok=True)
    files = {role: (out_dir / f"oasst1_{role}.txt").open("w", encoding="utf-8") for role in ("prompter", "assistant")}
    stats: Counter = Counter()
    try:
        for raw in lines:
            m = json.loads(raw)
            stats["messages"] += 1
            if not keep_message(m):
                continue
            if is_heldout_tree(m.get("message_tree_id", "")):
                stats["heldout_skipped"] += 1
                continue
            role = m["role"]
            paras = clean_markdown(str(m.get("text", "")))
            if not paras:
                continue
            stats[f"{role}_messages"] += 1
            for p in paras:
                stats[f"{role}_words"] += len(p.split())
                files[role].write(p + "\n")
    finally:
        for f in files.values():
            f.close()
    return stats


def heldout_prompter_paragraphs(path: Path) -> list[str]:
    """评测留出树中的英文用户提问段落（经与训练侧相同的过滤与 Markdown 清洗）。"""
    out: list[str] = []
    with gzip.open(path, "rt", encoding="utf-8") as f:
        for raw in f:
            m = json.loads(raw)
            if keep_message(m) and m["role"] == "prompter" and is_heldout_tree(m.get("message_tree_id", "")):
                out.extend(clean_markdown(str(m.get("text", ""))))
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--input", type=Path, default=DEFAULT_IN)
    ap.add_argument("--out-dir", type=Path, default=REPO_ROOT / "data/compiled/corpus")
    args = ap.parse_args(argv)
    if not args.input.exists():
        print(f"missing {args.input} (download per data/MANIFEST.yaml oasst1)", file=sys.stderr)
        return 1
    with gzip.open(args.input, "rt", encoding="utf-8") as f:
        stats = convert(f, args.out_dir)
    print(json.dumps(dict(sorted(stats.items())), ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
