#!/usr/bin/env python3
"""AnIME 开源英文词库构建脚本：合并 SCOWL 词形白名单与 wordfreq 现代词频，输出 TSV 与 ANIMELEX v1 二进制。"""

from __future__ import annotations

import argparse
import gzip
import re
import sys
import tarfile
import zipfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from common import (  # noqa: E402
    ENTRY_FLAG_CAPITALIZED,
    ENTRY_FLAG_CONTRACTION,
    ENTRY_FLAG_FROM_SCOWL,
    FLAG_CASE_PRESERVED,
    FLAG_PHONETIC_INDEX,
    LexiconEntry,
    MisspellingEntry,
    read_wordfreq_cbpack,
    write_animelex,
)

CMUDICT_VARIANT_RE = re.compile(r"\(\d+\)$")

SCOWL_FILE_RE = re.compile(
    r"^(?:english|american|british|british_z|canadian|australian)-"
    r"(?:words|contractions|proper-names|abbreviations)\.(\d+)$"
)
CLEAN_WORD_RE = re.compile(r"^[A-Za-z][A-Za-z'\-]{1,39}$")
SINGLE_LETTER_ALLOWED = {"a", "I"}

# 确保第一人称代词及相关缩写保留标准英文大小写
CANONICAL_CASE_OVERRIDES = {
    "i": "I",
    "i'm": "I'm",
    "i've": "I've",
    "i'll": "I'll",
    "i'd": "I'd",
}


def is_clean_english_word(word: str) -> bool:
    if len(word) == 1:
        return word in SINGLE_LETTER_ALLOWED
    if not CLEAN_WORD_RE.match(word):
        return False
    if "''" in word or "--" in word or word.endswith("'") or word.endswith("-"):
        return False
    return True


def _ingest_scowl_lines(
    filename: str,
    raw_bytes: bytes,
    max_level: int,
    scowl_forms: dict[str, str],
) -> None:
    m = SCOWL_FILE_RE.match(Path(filename).name)
    if not m:
        return
    level = int(m.group(1))
    if level > max_level:
        return

    text = raw_bytes.decode("latin-1", errors="ignore")
    for line in text.splitlines():
        word = line.strip()
        if not word or word.startswith("#") or not is_clean_english_word(word):
            continue
        low = word.lower()
        word = CANONICAL_CASE_OVERRIDES.get(low, word)
        existing = scowl_forms.get(low)
        if existing is None:
            scowl_forms[low] = word
        elif existing != word:
            # 若同时存在小写形式与大写形式（例如 "Apple" 与 "apple"），优先保留小写普通词形（"I" 例外）
            if low in CANONICAL_CASE_OVERRIDES:
                scowl_forms[low] = CANONICAL_CASE_OVERRIDES[low]
            elif word.islower() and not existing.islower():
                scowl_forms[low] = word


def load_scowl(
    scowl_tar: Path | None,
    scowl_dir: Path | None,
    max_level: int,
) -> dict[str, str]:
    scowl_forms: dict[str, str] = {}
    if scowl_tar is not None:
        with tarfile.open(scowl_tar, "r:*") as tf:
            for member in tf.getmembers():
                if not member.isfile():
                    continue
                f = tf.extractfile(member)
                if f is None:
                    continue
                _ingest_scowl_lines(member.name, f.read(), max_level, scowl_forms)
    if scowl_dir is not None:
        for path in sorted(scowl_dir.rglob("*")):
            if path.is_file():
                _ingest_scowl_lines(path.name, path.read_bytes(), max_level, scowl_forms)
    return scowl_forms


def load_wordfreq(
    wordfreq_whl: Path | None,
    wordfreq_tsv: Path | None,
) -> dict[str, float]:
    freqs: dict[str, float] = {}
    if wordfreq_whl is not None:
        with zipfile.ZipFile(wordfreq_whl, "r") as zf:
            names = zf.namelist()
            target_name = None
            for candidate in (
                "wordfreq/data/large_en.msgpack.gz",
                "wordfreq/data/small_en.msgpack.gz",
            ):
                if candidate in names:
                    target_name = candidate
                    break
            if target_name is None:
                en_files = sorted(n for n in names if n.endswith("en.msgpack.gz"))
                if en_files:
                    target_name = en_files[0]
            if target_name is None:
                raise ValueError(f"no English cBpack msgpack found inside {wordfreq_whl}")
            payload = gzip.decompress(zf.read(target_name))
            for k, v in read_wordfreq_cbpack(payload).items():
                low = k.strip().lower()
                if is_clean_english_word(k.strip()) or low in SINGLE_LETTER_ALLOWED:
                    if low not in freqs or v > freqs[low]:
                        freqs[low] = v

    if wordfreq_tsv is not None:
        for raw in wordfreq_tsv.read_text(encoding="utf-8").splitlines():
            s = raw.strip()
            if not s or s.startswith("#"):
                continue
            parts = raw.split("\t")
            if len(parts) < 2:
                continue
            if parts[0].strip().lower() == "word" and parts[1].strip().lower() in ("zipf", "freq"):
                continue
            w = parts[0].strip()
            low = w.lower()
            if not is_clean_english_word(w) and low not in SINGLE_LETTER_ALLOWED:
                continue
            zipf = float(parts[1].strip())
            if low not in freqs or zipf > freqs[low]:
                freqs[low] = zipf

    return freqs


def load_misspellings_tsv(path: Path, valid_targets_lower: set[str]) -> list[MisspellingEntry]:
    out: list[MisspellingEntry] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        s = raw.strip()
        if not s or s.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) < 2:
            continue
        typo = parts[0].strip().lower()
        target = parts[1].strip()
        if target.lower() not in valid_targets_lower:
            continue
        weight = float(parts[3].strip()) if len(parts) >= 4 and parts[3].strip() else 1.0
        out.append(MisspellingEntry(typo=typo, target=target, weight=weight))
    return out


def load_cmudict(path: Path) -> dict[str, str]:
    """解析开源 CMUdict 发音词典，去除重音数字并保留最多 3 个不重复读音变体（受 ANIMELEX u8 长度限制 <= 250B）。"""
    variants: dict[str, list[str]] = {}
    for raw in path.read_text(encoding="utf-8", errors="ignore").splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line or line.startswith(";;;"):
            continue
        parts = line.split()
        if len(parts) < 2:
            continue
        base = CMUDICT_VARIANT_RE.sub("", parts[0].lower())
        if not base:
            continue
        pron = " ".join(p.rstrip("012") for p in parts[1:] if p.rstrip("012"))
        if not pron:
            continue
        bucket = variants.setdefault(base, [])
        if pron not in bucket and len(bucket) < 3:
            bucket.append(pron)

    out: dict[str, str] = {}
    for word, prons in variants.items():
        # ANIMELEX v1 phonetic 字段使用单字节长度 (<= 255)，严格限制 <= 250 UTF-8 字节
        kept: list[str] = []
        total_bytes = 0
        for p in prons:
            p_bytes = len(p.encode("utf-8"))
            sep = 1 if kept else 0
            if total_bytes + sep + p_bytes > 250:
                break
            kept.append(p)
            total_bytes += sep + p_bytes
        if kept:
            out[word] = "|".join(kept)
    return out


def build_lexicon(
    scowl_tar: Path | None,
    scowl_dir: Path | None,
    max_scowl_level: int,
    wordfreq_whl: Path | None,
    wordfreq_tsv: Path | None,
    modern_min_zipf: float,
    misspellings_tsv: Path | None,
    cmudict_path: Path | None,
    max_words: int,
    out_tsv: Path,
    out_bin: Path,
) -> tuple[int, int]:
    scowl_forms = load_scowl(scowl_tar, scowl_dir, max_scowl_level)
    freqs = load_wordfreq(wordfreq_whl, wordfreq_tsv)
    cmudict_prons = load_cmudict(cmudict_path) if cmudict_path is not None else {}

    if not scowl_forms and not freqs:
        raise ValueError("empty input: neither SCOWL nor wordfreq produced any valid English words")

    # 1. 所有通过白名单的 SCOWL 词条进入候选，优先采用 wordfreq 现代词频
    candidates: list[tuple[str, float, bool]] = []
    seen_lower: set[str] = set()

    for low, canonical in scowl_forms.items():
        zipf = freqs.get(low, 1.5)
        candidates.append((canonical, zipf, True))
        seen_lower.add(low)

    # 2. 补充未入 SCOWL 但 Zipf >= modern_min_zipf 的现代高频新词（如 kubernetes / chatgpt）
    for low, zipf in freqs.items():
        if low in seen_lower or zipf < modern_min_zipf:
            continue
        canonical = CANONICAL_CASE_OVERRIDES.get(low, low)
        if not is_clean_english_word(canonical):
            continue
        candidates.append((canonical, zipf, False))
        seen_lower.add(low)

    candidates.sort(key=lambda item: (-item[1], item[0].lower(), item[0]))
    if max_words > 0:
        candidates = candidates[:max_words]

    entries: list[LexiconEntry] = []
    for idx, (word, zipf, from_scowl) in enumerate(candidates, start=1):
        eflags = 0
        if any(c.isupper() for c in word):
            eflags |= ENTRY_FLAG_CAPITALIZED
        if "'" in word:
            eflags |= ENTRY_FLAG_CONTRACTION
        if from_scowl:
            eflags |= ENTRY_FLAG_FROM_SCOWL
        entries.append(
            LexiconEntry(
                word=word,
                zipf=round(zipf, 2),
                freq_rank=idx,
                entry_flags=eflags,
                phonetic=cmudict_prons.get(word.lower(), ""),
            )
        )

    valid_lower = {e.word.lower() for e in entries}
    misspellings: list[MisspellingEntry] = []
    if misspellings_tsv is not None:
        misspellings = load_misspellings_tsv(misspellings_tsv, valid_lower)

    out_tsv.parent.mkdir(parents=True, exist_ok=True)
    with out_tsv.open("w", encoding="utf-8", newline="\n") as fh:
        fh.write("# word\tzipf\tfreq_rank\tentry_flags\tsource\n")
        for e in entries:
            src = "scowl+wordfreq" if (e.entry_flags & ENTRY_FLAG_FROM_SCOWL) else "wordfreq-modern"
            fh.write(f"{e.word}\t{e.zipf:.2f}\t{e.freq_rank}\t{e.entry_flags}\t{src}\n")

    bin_flags = FLAG_CASE_PRESERVED
    if cmudict_prons:
        bin_flags |= FLAG_PHONETIC_INDEX
    write_animelex(out_bin, entries, misspellings, flags=bin_flags)
    return len(entries), len(misspellings)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="合并开源 SCOWL 与 wordfreq 词频，生成词典 TSV 与 ANIMELEX v1 二进制。"
    )
    parser.add_argument("--scowl-tar", type=Path, default=None, help="SCOWL tar.gz 归档路径")
    parser.add_argument("--scowl-dir", type=Path, default=None, help="已解压的 SCOWL 目录路径")
    parser.add_argument(
        "--max-scowl-level",
        type=int,
        default=70,
        help="SCOWL 最大生僻度级别（默认 70）",
    )
    parser.add_argument("--wordfreq-whl", type=Path, default=None, help="wordfreq .whl 路径")
    parser.add_argument(
        "--wordfreq-tsv",
        type=Path,
        default=None,
        help="可选 wordfreq TSV（word\\tzipf），便于自定义或测试",
    )
    parser.add_argument(
        "--modern-min-zipf",
        type=float,
        default=3.0,
        help="允许作为现代新词补丁直接收录的最低 Zipf 词频（默认 3.0）",
    )
    parser.add_argument(
        "--misspellings-tsv",
        type=Path,
        default=None,
        help="可选由 build_misspellings.py 生成的错拼 TSV，一并打包进 ANIMELEX",
    )
    parser.add_argument(
        "--cmudict",
        type=Path,
        default=None,
        help="可选开源 CMUdict cmudict.dict 路径，用于填充 ANIMELEX phonetic 音素字段",
    )
    parser.add_argument(
        "--max-words",
        type=int,
        default=120000,
        help="最大收录词条数（默认 120000，0 表示不截断）",
    )
    parser.add_argument(
        "--out-tsv",
        type=Path,
        default=REPO_ROOT / "data" / "compiled" / "lexicon.tsv",
        help="输出词表 TSV 路径（默认 data/compiled/lexicon.tsv）",
    )
    parser.add_argument(
        "--out-bin",
        type=Path,
        default=REPO_ROOT / "data" / "compiled" / "lexicon.animelex.bin",
        help="输出 ANIMELEX 二进制路径（默认 data/compiled/lexicon.animelex.bin）",
    )
    args = parser.parse_args(argv)

    if not any([args.scowl_tar, args.scowl_dir, args.wordfreq_whl, args.wordfreq_tsv]):
        parser.error("至少需要提供 --scowl-tar / --scowl-dir / --wordfreq-whl / --wordfreq-tsv 之一")

    n_words, n_mis = build_lexicon(
        scowl_tar=args.scowl_tar,
        scowl_dir=args.scowl_dir,
        max_scowl_level=args.max_scowl_level,
        wordfreq_whl=args.wordfreq_whl,
        wordfreq_tsv=args.wordfreq_tsv,
        modern_min_zipf=args.modern_min_zipf,
        misspellings_tsv=args.misspellings_tsv,
        cmudict_path=args.cmudict,
        max_words=args.max_words,
        out_tsv=args.out_tsv,
        out_bin=args.out_bin,
    )
    print(
        f"lexicon build complete: {n_words} words, {n_mis} misspellings -> "
        f"{args.out_tsv} & {args.out_bin}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
