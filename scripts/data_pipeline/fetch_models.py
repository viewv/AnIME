#!/usr/bin/env python3
"""AnIME 开源 GGUF 端侧神经模型拉取与校验工具（复用 data/MANIFEST.yaml 白名单与 SHA256 校验）。"""

from __future__ import annotations

import argparse
import os
import sys
import urllib.parse
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from common import download_verified, load_and_validate_manifest, sha256_file  # noqa: E402


def is_model_source(source: dict) -> bool:
    url = source["source_url"].strip().lower()
    return url.endswith(".gguf") or source["id"].startswith("smollm2-")


def apply_hf_mirror(url: str) -> str:
    endpoint = os.environ.get("HF_ENDPOINT", "").strip().rstrip("/")
    if not endpoint:
        return url
    prefix = "https://huggingface.co"
    if url.startswith(prefix):
        return endpoint + url[len(prefix) :]
    return url


def model_filename(source: dict) -> str:
    parsed = urllib.parse.urlparse(source["source_url"])
    name = Path(parsed.path).name
    return name if name.endswith(".gguf") else f"{source['id']}.gguf"


def verify_gguf_magic(path: Path) -> None:
    with path.open("rb") as fh:
        magic = fh.read(4)
    if magic != b"GGUF":
        raise ValueError(f"{path}: invalid GGUF header magic {magic!r}, expected b'GGUF'")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="拉取并校验 data/MANIFEST.yaml 中登记的开源 GGUF 模型权重。"
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
        default=REPO_ROOT / "data" / "compiled" / "models",
        help="模型输出目录（默认 data/compiled/models）",
    )
    parser.add_argument(
        "--model",
        action="append",
        dest="models",
        default=[],
        help="指定下载或校验的 MANIFEST 模型 id（可多次传入）",
    )
    parser.add_argument(
        "--verify-only",
        action="store_true",
        help="仅校验 MANIFEST 与本地已有 GGUF 魔数/SHA256，不发起网络请求",
    )
    parser.add_argument(
        "--allow-unpinned",
        action="store_true",
        help="允许拉取 sha256=TBD 的模型并打印实际 SHA256 供锁定",
    )
    args = parser.parse_args(argv)

    manifest_path = args.manifest if args.manifest.is_absolute() else (REPO_ROOT / args.manifest)
    sources = load_and_validate_manifest(manifest_path, REPO_ROOT)
    model_sources = [s for s in sources if is_model_source(s)]
    by_id = {s["id"]: s for s in model_sources}

    if args.models:
        unknown = [mid for mid in args.models if mid not in by_id]
        if unknown:
            print(f"错误: MANIFEST 中未找到 GGUF 模型 id: {unknown}", file=sys.stderr)
            return 2
        selected = [by_id[mid] for mid in args.models]
    else:
        selected = model_sources

    if args.verify_only:
        print(f"MODEL MANIFEST OK: {len(model_sources)} 个开源 GGUF 模型通过协议与格式校验")
        for s in selected:
            dest = args.out_dir / model_filename(s)
            if dest.exists():
                verify_gguf_magic(dest)
                actual = sha256_file(dest)
                if s["sha256"] != "TBD" and actual != s["sha256"]:
                    print(
                        f"校验失败: {dest} SHA256 不匹配 (expected={s['sha256']}, actual={actual})",
                        file=sys.stderr,
                    )
                    return 1
            print(f"  - {s['id']} [{s['status']}] license={s['license']} file={dest.name}")
        return 0

    for s in selected:
        mid = s["id"]
        sha = s["sha256"].strip()
        if sha == "TBD" and not args.allow_unpinned:
            print(f"跳过未锁定模型 '{mid}' (sha256=TBD；请先通过 --allow-unpinned 拉取锁定)")
            continue
        url = apply_hf_mirror(s["source_url"].strip())
        dest = args.out_dir / model_filename(s)
        print(f"拉取 {mid}: {url} -> {dest}")
        path, actual = download_verified(
            url,
            dest,
            sha,
            allow_unpinned=args.allow_unpinned,
            timeout_sec=300,
        )
        verify_gguf_magic(path)
        size_mb = path.stat().st_size / (1024 * 1024)
        print(f"就绪 {mid}: {path} ({size_mb:.1f} MiB, sha256={actual})")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
