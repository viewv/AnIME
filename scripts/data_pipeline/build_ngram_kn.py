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


def extract_dialogue_text(json_paths: list[Path], out: Path) -> int:
    """MultiWOZ 风格对话 JSON（`[{"turns": [{"utterance": ...}]}]`）抽取全部轮次文本，每轮一行。"""
    n = 0
    with out.open("w", encoding="utf-8") as f:
        for p in sorted(json_paths):
            data = json.loads(p.read_text(encoding="utf-8"))
            for dlg in data if isinstance(data, list) else []:
                for turn in dlg.get("turns", []):
                    utt = " ".join(str(turn.get("utterance", "")).split())
                    if utt:
                        f.write(utt + "\n")
                        n += 1
    return n


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
    ap.add_argument(
        "--dialogue-glob",
        action="append",
        default=[],
        help="对话 JSON glob（相对仓库根，如 data/raw/multiwoz-2.2-train-*/dialogues_*.json）；严禁指向 test 分片",
    )
    ap.add_argument(
        "--dialogue-repeat",
        type=int,
        default=1,
        help="对话语料重复喂入次数：领域语料仅约 150 万词，不加权会在计数剪枝中几乎全部被裁掉",
    )
    ap.add_argument(
        "--weighted-text",
        action="append",
        default=[],
        metavar="PATH:REPEAT",
        help="纯文本语料及其重复喂入次数（如 data/compiled/corpus/oasst1_prompter.txt:100）；"
        "小语料不加权会在计数剪枝中几乎全部被裁掉，重复只放大原始计数，不改变 KN 续接计数",
    )
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
    weighted: list[tuple[Path, int]] = []
    for spec in args.weighted_text:
        path_s, _, rep_s = spec.rpartition(":")
        path = Path(path_s)
        if not path_s or not rep_s.isdigit() or int(rep_s) < 1 or not path.exists():
            print(f"invalid --weighted-text '{spec}' (expected existing PATH:REPEAT>=1)", file=sys.stderr)
            return 2
        weighted.append((path, int(rep_s)))
        cmd += ["--input", str(path)] * int(rep_s)
    dialogue_files = sorted({p for g in args.dialogue_glob for p in REPO_ROOT.glob(g)})
    if any("-test-" in str(p) for p in dialogue_files):
        print("refusing to train on MultiWOZ test shards (eval source)", file=sys.stderr)
        return 1
    n_turns = 0
    if dialogue_files:
        dialogue_txt = args.tmp_dir / "dialogue_turns.txt"
        n_turns = extract_dialogue_text(dialogue_files, dialogue_txt)
        print(f"dialogue corpus: {n_turns} turns from {len(dialogue_files)} files x{args.dialogue_repeat}", flush=True)
        cmd += ["--input", str(dialogue_txt)] * max(1, args.dialogue_repeat)

    proc = subprocess.run(cmd, stdin=stdin, check=False)
    if zcat is not None:
        zcat.wait()
    if proc.returncode != 0:
        return proc.returncode
    stats = json.loads(args.stats.read_text(encoding="utf-8"))
    stats["eval_exclusion_texts"] = n_ex
    stats["sources"] = [str(args.librispeech.name)] + [t.name for t in args.text] + [
        str(p.relative_to(REPO_ROOT)) for p in dialogue_files
    ]
    stats["dialogue_turns"] = n_turns
    stats["weighted_text"] = [{"path": p.name, "repeat": r} for p, r in weighted]
    stats["dialogue_repeat"] = args.dialogue_repeat
    args.stats.write_text(json.dumps(stats, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(stats, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
