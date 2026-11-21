#!/usr/bin/env python3
"""AnIME 开源数据源校验与受控拉取工具（严格校验 data/MANIFEST.yaml 开源协议白名单与 SHA256）。"""

from __future__ import annotations

import argparse
import sys
import urllib.parse
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from common import download_verified, load_and_validate_manifest, sha256_file  # noqa: E402


def infer_filename(source: dict) -> str:
    parsed = urllib.parse.urlparse(source["source_url"])
    basename = Path(parsed.path).name
    if basename and "." in basename and not basename.endswith(".php"):
        return basename
    sid = source["id"]
    if "wikipedia" in sid:
        return "wikipedia_for_machines.txt"
    return f"{sid}.dat"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="校验 AnIME data/MANIFEST.yaml 并按 SHA256 安全下载开源原始语料/词表。"
    )
    parser.add_argument(
        "--manifest",
        type=Path,
        default=REPO_ROOT / "data" / "MANIFEST.yaml",
        help="MANIFEST.yaml 路径（默认 data/MANIFEST.yaml）",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=REPO_ROOT / "data" / "raw",
        help="原始下载目录（默认 data/raw）",
    )
    parser.add_argument(
        "--verify-only",
        action="store_true",
        help="仅离线校验 MANIFEST 结构、协议白名单与构建脚本，不触发网络下载",
    )
    parser.add_argument(
        "--source",
        action="append",
        dest="sources",
        default=[],
        help="仅下载指定 id 的数据源（可多次传入）",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        help="下载 MANIFEST 中全部数据源（跳过未锁定 TBD 源除非指定 --allow-unpinned）",
    )
    parser.add_argument(
        "--allow-unpinned",
        action="store_true",
        help="允许下载 sha256=TBD 的 planned 源并打印实际 SHA256 供锁定",
    )
    args = parser.parse_args(argv)

    manifest_path = args.manifest if args.manifest.is_absolute() else (REPO_ROOT / args.manifest)
    sources = load_and_validate_manifest(manifest_path, REPO_ROOT)
    by_id = {s["id"]: s for s in sources}

    if args.sources:
        unknown = [sid for sid in args.sources if sid not in by_id]
        if unknown:
            print(f"错误: MANIFEST 中不存在数据源 id: {unknown}", file=sys.stderr)
            return 2

    if args.verify_only or (not args.all and not args.sources):
        print(f"MANIFEST OK: {len(sources)} 个数据源全部通过开源协议与格式校验")
        for s in sources:
            dest = args.out_dir / s["id"] / infer_filename(s)
            if dest.exists() and s["sha256"] != "TBD":
                actual = sha256_file(dest)
                if actual != s["sha256"]:
                    print(
                        f"校验失败: 本地缓存 {dest} SHA256 不匹配 (expected={s['sha256']}, actual={actual})",
                        file=sys.stderr,
                    )
                    return 1
            print(f"  - {s['id']} [{s['status']}] license={s['license']} sha256={s['sha256'][:12]}...")
        return 0

    selected = [by_id[sid] for sid in args.sources] if args.sources else sources
    for s in selected:
        sid = s["id"]
        sha = s["sha256"].strip()
        if sha == "TBD" and not args.allow_unpinned:
            print(f"跳过未锁定源 '{sid}' (sha256=TBD；如须拉取锁定请加 --allow-unpinned)")
            continue
        dest = args.out_dir / sid / infer_filename(s)
        print(f"拉取 {sid} -> {dest}")
        path, actual = download_verified(
            s["source_url"],
            dest,
            sha,
            allow_unpinned=args.allow_unpinned,
        )
        print(f"就绪 {sid}: {path} (sha256={actual})")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
