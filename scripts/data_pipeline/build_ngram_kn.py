#!/usr/bin/env python3
"""AnIME L2 语言模型构建（修正 Kneser-Ney，全量语料 + 评测去泄漏）。

编排步骤：
1. 汇总 `eval/**/*.tsv`（及本机 `data/compiled/eval_large_open.tsv`）的上下文与全部参考答案，写成
   去泄漏参照句表；训练语料中与之整句相同或共享任一 5-gram 的句子会被 `anime-ngram-build` 丢弃。
2. 以 L1 词库词形作为强制保留词表，保证候选词都有统计。
3. 以流式方式把语料喂给 Rust 构建器（`crates/lm-ngram/src/bin/anime-ngram-build.rs`），
   由其完成归一化、外部排序计数、插值修正 KN 平滑、按体积预算剪枝并写出 `ANIMENGR` v1。

旧版 `build_ngram.py`（加一平滑 + 常数回退）保留用于小规模测试与 ARPA 导入。
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

DEFAULT_LIBRISPEECH = REPO_ROOT / "data/raw/ngram-corpus/librispeech-lm-norm.txt.gz"


def eval_tsv_paths(repo_root: Path) -> list[Path]:
    paths = sorted((repo_root / "eval").rglob("*.tsv"))
    local = repo_root / "data/compiled/eval_large_open.tsv"
    if local.exists():
        paths.append(local)
    return paths


def collect_eval_texts(paths: list[Path]) -> list[str]:
    """评测集中所有可能被模型"背下来"的干净文本：上下文与每个参考答案。"""
    texts: list[str] = []
    for p in paths:
        for raw in p.read_text(encoding="utf-8").splitlines():
            if not raw.strip() or raw.lstrip().startswith("#"):
                continue
            parts = raw.split("\t")
            if len(parts) != 5:
                continue
            ctx, expected = parts[1].strip(), parts[3]
            if ctx:
                texts.append(ctx)
            for ref in expected.split("||"):
                ref = ref.strip()
                if ref:
                    texts.append(ref)
    return texts


def write_exclusion_file(texts: list[str], out: Path) -> int:
    uniq = sorted(set(texts))
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(uniq) + "\n", encoding="utf-8")
    return len(uniq)


def write_keep_vocab(lexicon_tsv: Path, out: Path) -> int:
    words: set[str] = set()
    for raw in lexicon_tsv.read_text(encoding="utf-8").splitlines():
        if raw and not raw.startswith("#"):
            words.add(raw.split("\t", 1)[0].lower())
    out.write_text("\n".join(sorted(words)) + "\n", encoding="utf-8")
    return len(words)


def build_builder(cargo_target_dir: Path | None) -> Path:
    cmd = ["cargo", "build", "--release", "-p", "anime-lm-ngram", "--bin", "anime-ngram-build"]
    subprocess.run(cmd, cwd=REPO_ROOT, check=True)
    target = cargo_target_dir or REPO_ROOT / "target"
    return target / "release" / "anime-ngram-build"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--librispeech", type=Path, default=DEFAULT_LIBRISPEECH, help="LibriSpeech LM .txt.gz（全量）")
    ap.add_argument("--text", type=Path, action="append", default=[], help="额外纯文本语料（每行一句或一段）")
    ap.add_argument("--lexicon-tsv", type=Path, default=REPO_ROOT / "data/compiled/lexicon.tsv")
    ap.add_argument("--out", type=Path, default=REPO_ROOT / "data/compiled/ngram.animengr.bin")
    ap.add_argument("--stats", type=Path, default=REPO_ROOT / "data/compiled/ngram_stats.json")
    ap.add_argument("--tmp-dir", type=Path, default=REPO_ROOT / "data/compiled/ngram_build_tmp")
    ap.add_argument("--vocab-size", type=int, default=250_000)
    ap.add_argument("--max-bigrams", type=int, default=5_000_000)
    ap.add_argument("--max-trigrams", type=int, default=7_000_000)
    ap.add_argument("--min-trigram-count", type=int, default=2)
    ap.add_argument("--chunk-entries", type=int, default=40_000_000)
    ap.add_argument("--cargo-target-dir", type=Path, default=None)
    args = ap.parse_args(argv)

    args.tmp_dir.mkdir(parents=True, exist_ok=True)
    exclude = args.tmp_dir / "eval_exclusion.txt"
    keep = args.tmp_dir / "keep_vocab.txt"
    eval_paths = eval_tsv_paths(REPO_ROOT)
    n_ex = write_exclusion_file(collect_eval_texts(eval_paths), exclude)
    n_keep = write_keep_vocab(args.lexicon_tsv, keep)
    print(f"eval exclusion: {n_ex} texts from {len(eval_paths)} TSV; keep-vocab: {n_keep} words", flush=True)

    builder = build_builder(args.cargo_target_dir)
    cmd = [
        str(builder),
        "--exclude", str(exclude),
        "--keep-vocab", str(keep),
        "--vocab-size", str(args.vocab_size),
        "--max-bigrams", str(args.max_bigrams),
        "--max-trigrams", str(args.max_trigrams),
        "--min-trigram-count", str(args.min_trigram_count),
        "--chunk-entries", str(args.chunk_entries),
        "--tmp-dir", str(args.tmp_dir),
        "--out", str(args.out),
        "--stats", str(args.stats),
    ]
    stdin = None
    zcat = None
    if args.librispeech:
        if not args.librispeech.exists():
            print(f"missing corpus {args.librispeech} (run fetch_sources.py)", file=sys.stderr)
            return 1
        zcat = subprocess.Popen(["gzip", "-dc", str(args.librispeech)], stdout=subprocess.PIPE)
        stdin = zcat.stdout
        cmd += ["--input", "-"]
    for t in args.text:
        cmd += ["--input", str(t)]

    proc = subprocess.run(cmd, stdin=stdin, check=False)
    if zcat is not None:
        zcat.wait()
    if proc.returncode != 0:
        return proc.returncode
    stats = json.loads(args.stats.read_text(encoding="utf-8"))
    stats["eval_exclusion_texts"] = n_ex
    stats["sources"] = [str(args.librispeech.name)] + [t.name for t in args.text]
    args.stats.write_text(json.dumps(stats, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(stats, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
