#!/usr/bin/env python3
"""Turn harvested arXiv OAI pages into clean CS abstract text with a per-paper held-out split.

- Keeps only papers whose *primary* category is cs.* (the cs set also contains cross-lists).
- Abstract LaTeX is flattened: inline / display math becomes a clause break (math is not typed
  through the IME), `\\cite{..}`-style commands are dropped, `\\emph{x}` keeps `x`, `~` -> space.
- Titles are not emitted: Title Case would teach the models wrong capitalization.
- Held-out split: CRC32("arxiv:" + id) % 100 < HELDOUT_PCT. Held-out papers feed only
  eval/benchmarks/ood_academic_v1.tsv; every training consumer must read train.jsonl.

Output: data/compiled/arxiv_cs/{train,heldout}.jsonl with {"id","cats","abstract"}.
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
from xml.etree import ElementTree as ET

REPO_ROOT = Path(__file__).resolve().parents[2]
NS = {"oai": "http://www.openarchives.org/OAI/2.0/", "ax": "http://arxiv.org/OAI/arXiv/"}
HELDOUT_PCT = 10

_MATH_RE = re.compile(r"\$\$.*?\$\$|\$[^$]*\$|\\\[.*?\\\]|\\\(.*?\\\)", re.S)
_KEEP_ARG_RE = re.compile(r"\\(?:emph|textit|textbf|text|textrm|texttt|mathrm|underline)\s*\{([^{}]*)\}")
_DROP_CMD_RE = re.compile(r"\\[A-Za-z]+\*?(?:\[[^\]]*\])?(?:\{[^{}]*\})?")
_URL_RE = re.compile(r"https?://\S+|www\.\S+")
_WS_RE = re.compile(r"\s+")


def is_heldout(arxiv_id: str) -> bool:
    return zlib.crc32(f"arxiv:{arxiv_id}".encode()) % 100 < HELDOUT_PCT


def clean_abstract(text: str) -> str:
    t = _MATH_RE.sub(" ; ", text)
    t = _KEEP_ARG_RE.sub(r"\1", t)
    t = _DROP_CMD_RE.sub(" ", t)
    t = _URL_RE.sub(" ; ", t)
    t = t.replace("~", " ").replace("{", "").replace("}", "").replace("\\", " ")
    t = t.replace("``", '"').replace("''", '"')
    return _WS_RE.sub(" ", t).strip()


def iter_records(raw_dir: Path):
    for page in sorted(raw_dir.glob("page-*.xml.gz")):
        with gzip.open(page) as f:
            root = ET.fromstring(f.read())
        for rec in root.iter(f"{{{NS['oai']}}}record"):
            md = rec.find("oai:metadata/ax:arXiv", NS)
            if md is None:
                continue
            yield {
                "id": (md.findtext("ax:id", "", NS) or "").strip(),
                "cats": (md.findtext("ax:categories", "", NS) or "").split(),
                "abstract": md.findtext("ax:abstract", "", NS) or "",
            }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--raw-dir", type=Path, default=REPO_ROOT / "data/raw/arxiv-cs")
    ap.add_argument("--out-dir", type=Path, default=REPO_ROOT / "data/compiled/arxiv_cs")
    args = ap.parse_args(argv)
    if not any(args.raw_dir.glob("page-*.xml.gz")):
        print(f"no pages in {args.raw_dir} (run fetch_arxiv_oai.py)", file=sys.stderr)
        return 1
    args.out_dir.mkdir(parents=True, exist_ok=True)
    stats: Counter = Counter()
    seen: set[str] = set()
    with open(args.out_dir / "train.jsonl", "w") as ftr, open(args.out_dir / "heldout.jsonl", "w") as fho:
        for r in iter_records(args.raw_dir):
            stats["records"] += 1
            if not r["id"] or r["id"] in seen:
                stats["dup"] += 1
                continue
            seen.add(r["id"])
            if not r["cats"] or not r["cats"][0].startswith("cs."):
                stats["not_primary_cs"] += 1
                continue
            abstract = clean_abstract(r["abstract"])
            if len(abstract.split()) < 20:
                stats["short"] += 1
                continue
            out = {"id": r["id"], "cats": r["cats"], "abstract": abstract}
            if is_heldout(r["id"]):
                fho.write(json.dumps(out, ensure_ascii=False) + "\n")
                stats["heldout"] += 1
            else:
                ftr.write(json.dumps(out, ensure_ascii=False) + "\n")
                stats["train"] += 1
    (args.out_dir / "stats.json").write_text(json.dumps(dict(sorted(stats.items())), indent=1))
    print(json.dumps(dict(sorted(stats.items()))))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
