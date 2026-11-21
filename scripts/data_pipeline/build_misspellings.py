#!/usr/bin/env python3
"""AnIME 开源常见拼写错误映射构建脚本：合并 Wikipedia For_machines / crate-ci/typos / TSV。"""

from __future__ import annotations

import argparse
import csv
import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from common import in_split  # noqa: E402

TOKEN_RE = re.compile(r"^[A-Za-z][A-Za-z'\-]{0,38}[A-Za-z]$|^[A-Za-z]$")


def is_clean_token(token: str) -> bool:
    return bool(TOKEN_RE.match(token)) and ("''" not in token) and ("--" not in token)


def parse_wiki_for_machines(path: Path) -> list[tuple[str, str, str, float]]:
    entries: list[tuple[str, str, str, float]] = []
    in_pre = False
    has_pre = False
    raw_lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    for line in raw_lines:
        if "<pre" in line:
            has_pre = True
            in_pre = True
            continue
        if "</pre>" in line:
            in_pre = False
            continue
        if has_pre and not in_pre:
            continue
        s = line.strip()
        if not s or s.startswith("#") or "->" not in s:
            continue
        left, right = s.split("->", 1)
        typo = left.strip().lower()
        if not is_clean_token(typo):
            continue
        targets = [t.strip() for t in right.split(",") if t.strip()]
        for idx, target in enumerate(targets):
            # Wikipedia 偶尔含括号注释或短语，只保留词级规范映射
            target_clean = target.strip()
            if not is_clean_token(target_clean):
                continue
            if typo == target_clean.lower():
                continue
            weight = max(0.5, round(1.0 - 0.1 * idx, 2))
            entries.append((typo, target_clean, "wikipedia", weight))
    return entries


def parse_typos_csv(path: Path) -> list[tuple[str, str, str, float]]:
    entries: list[tuple[str, str, str, float]] = []
    with path.open("r", encoding="utf-8", errors="replace", newline="") as fh:
        reader = csv.reader(fh)
        for row in reader:
            if not row:
                continue
            typo = row[0].strip().lower()
            if not typo or typo.startswith("#") or typo == "typo":
                continue
            if not is_clean_token(typo):
                continue
            for idx, raw_target in enumerate(row[1:]):
                target = raw_target.strip()
                if not is_clean_token(target):
                    continue
                if typo == target.lower():
                    continue
                weight = max(0.5, round(0.95 - 0.1 * idx, 2))
                entries.append((typo, target, "crate-ci-typos", weight))
    return entries


def parse_plain_tsv(path: Path) -> list[tuple[str, str, str, float]]:
    entries: list[tuple[str, str, str, float]] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        s = raw.strip()
        if not s or s.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) < 2:
            continue
        typo = parts[0].strip().lower()
        target = parts[1].strip()
        if typo == "typo" and target.lower() in ("correction", "target"):
            continue
        if not is_clean_token(typo) or not is_clean_token(target):
            continue
        if typo == target.lower():
            continue
        src = parts[2].strip() if len(parts) >= 3 and parts[2].strip() else "custom-tsv"
        weight = 1.0
        if len(parts) >= 4 and parts[3].strip():
            weight = float(parts[3].strip())
        entries.append((typo, target, src, weight))
    return entries


def load_lexicon_whitelist(path: Path) -> set[str]:
    allowed: set[str] = set()
    for raw in path.read_text(encoding="utf-8").splitlines():
        s = raw.strip()
        if not s or s.startswith("#"):
            continue
        word = raw.split("\t", 1)[0].strip().lower()
        if word:
            allowed.add(word)
    return allowed


def build_misspellings(
    wiki_paths: list[Path],
    typos_csv_paths: list[Path],
    tsv_paths: list[Path],
    lexicon_tsv: Path | None,
    out_tsv: Path,
    split: str = "all",
    eval_pct: int = 20,
) -> int:
    raw_entries: list[tuple[str, str, str, float]] = []
    for p in wiki_paths:
        raw_entries.extend(parse_wiki_for_machines(p))
    for p in typos_csv_paths:
        raw_entries.extend(parse_typos_csv(p))
    for p in tsv_paths:
        raw_entries.extend(parse_plain_tsv(p))

    whitelist = load_lexicon_whitelist(lexicon_tsv) if lexicon_tsv else None

    # 去重聚合：(typo, target) -> (sources_set, max_weight)
    merged: dict[tuple[str, str], tuple[set[str], float]] = {}
    for typo, target, src, weight in raw_entries:
        if not in_split(typo, split, eval_pct):
            continue
        if whitelist is not None and target.lower() not in whitelist:
            continue
        key = (typo, target)
        if key not in merged:
            merged[key] = ({src}, weight)
        else:
            srcs, old_w = merged[key]
            srcs.add(src)
            if weight > old_w:
                merged[key] = (srcs, weight)

    rows = [
        (typo, target, "+".join(sorted(srcs)), weight)
        for (typo, target), (srcs, weight) in merged.items()
    ]
    rows.sort(key=lambda r: (r[0], -r[3], r[1]))

    out_tsv.parent.mkdir(parents=True, exist_ok=True)
    with out_tsv.open("w", encoding="utf-8", newline="\n") as fh:
        fh.write("# typo\tcorrection\tsource\tweight\n")
        for typo, target, src, weight in rows:
            fh.write(f"{typo}\t{target}\t{src}\t{weight:.2f}\n")
    return len(rows)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="清洗并合并开源英文错拼映射表，输出 UTF-8 TSV（typo\\tcorrection\\tsource\\tweight）。"
    )
    parser.add_argument(
        "--wiki-raw",
        action="append",
        type=Path,
        default=[],
        help="Wikipedia Lists_of_common_misspellings/For_machines 原始文本路径",
    )
    parser.add_argument(
        "--typos-csv",
        action="append",
        type=Path,
        default=[],
        help="crate-ci/typos 词表 CSV 路径（MIT OR Apache-2.0）",
    )
    parser.add_argument(
        "--tsv",
        action="append",
        type=Path,
        default=[],
        help="额外自定义错拼 TSV（typo\\tcorrection[\\tsource[\\tweight]]）",
    )
    parser.add_argument(
        "--lexicon-tsv",
        type=Path,
        default=None,
        help="可选合法词库 TSV（仅保留合法目标词）",
    )
    parser.add_argument(
        "--split",
        choices=("all", "train", "eval"),
        default="all",
        help="防泄漏切分模式（train 用于入库，eval 留作评测，默认 all）",
    )
    parser.add_argument(
        "--eval-pct",
        type=int,
        default=20,
        help="eval 切分占比百分比（默认 20）",
    )
    parser.add_argument(
        "--out-tsv",
        type=Path,
        default=REPO_ROOT / "data" / "compiled" / "misspellings.tsv",
        help="输出错拼 TSV 路径（默认 data/compiled/misspellings.tsv）",
    )
    args = parser.parse_args(argv)

    if not (args.wiki_raw or args.typos_csv or args.tsv):
        parser.error("至少需要提供一个输入源（--wiki-raw / --typos-csv / --tsv）")

    count = build_misspellings(
        wiki_paths=args.wiki_raw,
        typos_csv_paths=args.typos_csv,
        tsv_paths=args.tsv,
        lexicon_tsv=args.lexicon_tsv,
        out_tsv=args.out_tsv,
        split=args.split,
        eval_pct=args.eval_pct,
    )
    print(f"misspellings build complete: {count} entries -> {args.out_tsv}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
