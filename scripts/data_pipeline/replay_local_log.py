#!/usr/bin/env python3
"""在本机回放用户自己的打字记录：转换为评测 TSV 并调用 anime-eval，只输出聚合指标。

输入格式（UTF-8 TSV，`#` 开头为注释），每行一次上屏：

    context_before<TAB>typed<TAB>committed

- `context_before`：上屏前光标前文（可空）；
- `typed`：用户在预编辑区实际敲出的字母/空格串（已含其本人退格修正后的结果）；
- `committed`：用户最终上屏的文本（视为期望答案）。

隐私约束：记录与转换结果只留在本机 `data/compiled/local/`（已被 .gitignore），不得写入 `eval/`、
不进入任何训练语料；默认只打印聚合指标，不输出任何样本原文。
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
DEFAULT_OUT = REPO_ROOT / "data/compiled/local/local_eval.tsv"


def categorize(typed: str, committed: str) -> str:
    t, c = typed.strip().lower(), committed.strip().lower()
    if " " in t or " " in c:
        return "sentence"
    if t == c:
        return "exact"
    if c.startswith(t):
        return "prefix"
    return "typo_simple"


def convert(lines: list[str]) -> tuple[list[str], int]:
    rows: list[str] = []
    skipped = 0
    for raw in lines:
        if not raw.strip() or raw.startswith("#"):
            continue
        parts = raw.rstrip("\n").split("\t")
        if len(parts) != 3:
            skipped += 1
            continue
        ctx, typed, committed = parts
        typed, committed = typed.strip(), committed.strip()
        if not typed or not committed or "||" in committed or any("\t" in x for x in parts):
            skipped += 1
            continue
        rows.append("\t".join([categorize(typed, committed), ctx, typed, committed, "source:local"]))
    return rows, skipped


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("log", type=Path, help="本机打字记录 TSV")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--lexicon", type=Path, default=REPO_ROOT / "data/compiled/lexicon.animelex.bin")
    ap.add_argument("--ngram", type=Path, default=REPO_ROOT / "data/compiled/ngram.animengr.bin")
    ap.add_argument("--anime-eval", type=Path, default=REPO_ROOT / "target/release/anime-eval")
    ap.add_argument("--convert-only", action="store_true")
    args = ap.parse_args(argv)

    out = args.out.resolve()
    if (REPO_ROOT / "eval").resolve() in out.parents:
        print("refusing to write local typing data under eval/ (it would be committed)", file=sys.stderr)
        return 2
    rows, skipped = convert(args.log.read_text(encoding="utf-8").splitlines())
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("# 本机打字记录转换结果（勿入库）\n" + "\n".join(rows) + ("\n" if rows else ""), encoding="utf-8")
    print(f"converted {len(rows)} commits ({skipped} malformed lines skipped) -> {out}")
    if args.convert_only or not rows:
        return 0
    cmd = [
        str(args.anime_eval), str(out),
        "--lexicon", str(args.lexicon), "--ngram", str(args.ngram),
        "--interactive",
    ]
    return subprocess.run(cmd, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
