#!/usr/bin/env python3
"""Harvest a bounded subset of arXiv CS metadata (title + abstract) via OAI-PMH.

Why bounded / slow: arXiv Terms of Use allow descriptive metadata under CC0 but ask
for at most one request every 3 seconds on a single connection, and forbid
programmatic download of the complete corpus. We fetch only `set=cs` from a fixed
datestamp and stop at --max-records.

Each OAI page is stored verbatim as data/raw/arxiv-cs/page-NNNNN.xml.gz; the
resumption token is kept in state.json so an interrupted run resumes in place.
"""
from __future__ import annotations

import argparse
import gzip
import json
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
BASE_URL = "https://oaipmh.arxiv.org/oai"
USER_AGENT = "AnIME-data-pipeline/0.1 (offline research harvest; 1 req / 3 s)"
TOKEN_RE = re.compile(r"<resumptionToken[^>]*>([^<]*)</resumptionToken>")
RECORD_RE = re.compile(r"<record>")


def fetch(url: str, retries: int = 8) -> bytes:
    for attempt in range(retries):
        req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(req, timeout=120) as resp:
                return resp.read()
        except urllib.error.HTTPError as e:
            if e.code == 503:
                wait = int(e.headers.get("Retry-After", "30") or 30)
            elif e.code >= 500:
                wait = 30 * (attempt + 1)
            else:
                raise
        except (urllib.error.URLError, TimeoutError):
            wait = 30 * (attempt + 1)
        print(f"  retry in {wait}s (attempt {attempt + 1})", file=sys.stderr)
        time.sleep(wait)
    raise RuntimeError(f"giving up on {url}")


def write_page_listing(out_dir: Path) -> Path:
    """`sha256  page-NNNNN.xml.gz` 清单；MANIFEST 的 sha256 锁定的是这份清单（fetch_sources 按 <id>.dat 校验）。"""
    import hashlib

    lines = [f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}" for p in sorted(out_dir.glob("page-*.xml.gz"))]
    listing = out_dir / "arxiv-cs.dat"
    listing.write_text("\n".join(lines) + "\n")
    return listing


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out-dir", type=Path, default=REPO_ROOT / "data/raw/arxiv-cs")
    ap.add_argument("--set", default="cs")
    ap.add_argument("--from-date", default="2025-01-01")
    ap.add_argument("--max-records", type=int, default=65_000)
    ap.add_argument("--delay", type=float, default=3.5)
    args = ap.parse_args(argv)

    args.out_dir.mkdir(parents=True, exist_ok=True)
    state_path = args.out_dir / "state.json"
    state = json.loads(state_path.read_text()) if state_path.exists() else {
        "set": args.set, "from": args.from_date, "pages": 0, "records": 0, "token": None, "done": False,
    }
    if state["done"]:
        write_page_listing(args.out_dir)
        print(f"already complete: {state['pages']} pages, {state['records']} records")
        return 0

    while state["records"] < args.max_records:
        if state["token"]:
            q = {"verb": "ListRecords", "resumptionToken": state["token"]}
        else:
            q = {"verb": "ListRecords", "metadataPrefix": "arXiv", "set": state["set"], "from": state["from"]}
        body = fetch(f"{BASE_URL}?{urllib.parse.urlencode(q)}")
        n = len(RECORD_RE.findall(body.decode("utf-8", "replace")))
        page = args.out_dir / f"page-{state['pages']:05d}.xml.gz"
        with gzip.open(page, "wb") as f:
            f.write(body)
        m = TOKEN_RE.search(body.decode("utf-8", "replace"))
        state["pages"] += 1
        state["records"] += n
        state["token"] = m.group(1).strip() if m and m.group(1).strip() else None
        state["done"] = state["token"] is None
        state_path.write_text(json.dumps(state, indent=1))
        print(f"page {state['pages']}: +{n} records (total {state['records']})", flush=True)
        if state["done"]:
            break
        time.sleep(args.delay)

    state["done"] = True
    state_path.write_text(json.dumps(state, indent=1))
    write_page_listing(args.out_dir)
    print(f"finished: {state['pages']} pages, {state['records']} records")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
