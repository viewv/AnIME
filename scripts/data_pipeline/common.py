#!/usr/bin/env python3
"""AnIME 开源数据管线公共工具：清单校验、安全下载、纯标准库 msgpack 解析与 ANIMELEX/ANIMENGR 二进制编解码。"""

from __future__ import annotations

import hashlib
import os
import re
import struct
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import yaml

# 对齐 .agents/contracts/data-format.md §3 的 16 字节固定头约定
MAGIC_LEX = b"ANIMELEX"
MAGIC_NGR = b"ANIMENGR"
FORMAT_VERSION = 1

# Header feature flags (data-format.md §3)
FLAG_CASE_PRESERVED = 1 << 0
FLAG_PHONETIC_INDEX = 1 << 1
FLAG_COMPACT_QUANTIZED = 1 << 2

# ANIMELEX 词条标记位
ENTRY_FLAG_CAPITALIZED = 1 << 0
ENTRY_FLAG_CONTRACTION = 1 << 1
ENTRY_FLAG_FROM_SCOWL = 1 << 2

HEADER_STRUCT = struct.Struct("<8sII")

REQUIRED_MANIFEST_FIELDS = (
    "id",
    "status",
    "description",
    "source_url",
    "version",
    "license",
    "sha256",
    "build_script",
    "notes",
)

ALLOWED_LICENSE_TOKENS = (
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSD-like",
    "MPL-2.0",
    "ISC",
    "CC0-1.0",
    "Public Domain",
    "CC-BY-4.0",
    "CC-BY-SA-4.0",
)

FORBIDDEN_LICENSE_PATTERNS = (
    re.compile(r"(?<![A-Za-z])NC(?![A-Za-z])", re.IGNORECASE),
    re.compile(r"Non-?Commercial", re.IGNORECASE),
    re.compile(r"(?<![A-Za-z])(?:A|L)?GPL(?:-[0-9.]+)?(?![A-Za-z])", re.IGNORECASE),
    re.compile(r"Proprietary", re.IGNORECASE),
    re.compile(r"Closed[- ]?Source", re.IGNORECASE),
    re.compile(r"All Rights Reserved", re.IGNORECASE),
)

SHA256_HEX_RE = re.compile(r"^[0-9a-f]{64}$")


@dataclass(frozen=True)
class LexiconEntry:
    word: str
    zipf: float
    freq_rank: int
    entry_flags: int = 0
    phonetic: str = ""


@dataclass(frozen=True)
class MisspellingEntry:
    typo: str
    target: str
    weight: float = 1.0


def pack_header(magic: bytes, version: int = FORMAT_VERSION, flags: int = 0) -> bytes:
    if len(magic) != 8:
        raise ValueError(f"magic must be 8 bytes, got {len(magic)}")
    return HEADER_STRUCT.pack(magic, version, flags)


def unpack_header(buf: bytes | memoryview, expected_magic: bytes) -> tuple[int, int]:
    if len(buf) < HEADER_STRUCT.size:
        raise ValueError(
            f"binary header too short: expected >= {HEADER_STRUCT.size} bytes, got {len(buf)}"
        )
    magic, version, flags = HEADER_STRUCT.unpack_from(buf, 0)
    if magic != expected_magic:
        raise ValueError(f"invalid magic: expected {expected_magic!r}, got {magic!r}")
    if version != FORMAT_VERSION:
        raise ValueError(f"unsupported format version {version}, expected {FORMAT_VERSION}")
    return version, flags


def validate_license_string(license_str: str) -> None:
    text = (license_str or "").strip()
    if not text:
        raise ValueError("license field must not be empty")
    for pat in FORBIDDEN_LICENSE_PATTERNS:
        if pat.search(text):
            raise ValueError(f"license '{text}' hits forbidden pattern: {pat.pattern}")
    lowered = text.lower()
    if not any(tok.lower() in lowered for tok in ALLOWED_LICENSE_TOKENS):
        raise ValueError(
            f"license '{text}' is not in open-source whitelist: {ALLOWED_LICENSE_TOKENS}"
        )


def load_and_validate_manifest(manifest_path: Path, repo_root: Path) -> list[dict[str, Any]]:
    raw = yaml.safe_load(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        raise ValueError(f"{manifest_path}: root must be a YAML mapping")
    if raw.get("version") != 1:
        raise ValueError(f"{manifest_path}: version must be 1, got {raw.get('version')!r}")
    sources = raw.get("sources")
    if not isinstance(sources, list) or not sources:
        raise ValueError(f"{manifest_path}: 'sources' must be a non-empty list")

    seen_ids: set[str] = set()
    for idx, src in enumerate(sources):
        if not isinstance(src, dict):
            raise ValueError(f"{manifest_path}: sources[{idx}] must be a mapping")
        for field in REQUIRED_MANIFEST_FIELDS:
            if field not in src or not isinstance(src[field], str) or not src[field].strip():
                raise ValueError(
                    f"{manifest_path}: sources[{idx}] missing or empty string field '{field}'"
                )

        sid = src["id"].strip()
        if sid in seen_ids:
            raise ValueError(f"{manifest_path}: duplicate source id '{sid}'")
        seen_ids.add(sid)

        status = src["status"].strip()
        if status not in ("planned", "ready"):
            raise ValueError(f"{manifest_path}: source '{sid}' has invalid status '{status}'")

        url = src["source_url"].strip()
        if not (url.startswith("https://") or url.startswith("http://")):
            raise ValueError(f"{manifest_path}: source '{sid}' source_url must be http(s)://")

        validate_license_string(src["license"])

        sha = src["sha256"].strip()
        if status == "ready":
            if not SHA256_HEX_RE.match(sha):
                raise ValueError(
                    f"{manifest_path}: ready source '{sid}' must have 64-char lowercase hex sha256, got {sha!r}"
                )
        else:
            if sha != "TBD" and not SHA256_HEX_RE.match(sha):
                raise ValueError(
                    f"{manifest_path}: planned source '{sid}' sha256 must be 'TBD' or 64-char hex, got {sha!r}"
                )

        script_rel = src["build_script"].strip()
        if not (repo_root / script_rel).is_file():
            raise ValueError(
                f"{manifest_path}: source '{sid}' build_script not found: {script_rel}"
            )

    return sources


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        while True:
            chunk = fh.read(1024 * 1024)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()


def deterministic_split_bucket(key: str) -> int:
    """将任意键稳定映射到 [0, 99]，用于训练集与 held-out 评测集的防泄漏切分。"""
    digest = hashlib.sha256(key.encode("utf-8")).hexdigest()
    return int(digest[:8], 16) % 100


def in_split(key: str, split: str, eval_pct: int = 20) -> bool:
    """按比例划入 all / train / eval 切分，确保 train 与 eval 零重叠。"""
    if split == "all":
        return True
    if not (0 <= eval_pct <= 100):
        raise ValueError(f"eval_pct must be in 0..=100, got {eval_pct}")
    bucket = deterministic_split_bucket(key)
    cutoff = 100 - eval_pct
    if split == "train":
        return bucket < cutoff
    if split == "eval":
        return bucket >= cutoff
    raise ValueError(f"unknown split '{split}' (expected all, train, or eval)")


def download_verified(
    url: str,
    dest: Path,
    expected_sha256: str,
    allow_unpinned: bool = False,
    timeout_sec: int = 60,
) -> tuple[Path, str]:
    """下载文件到本地并做 SHA256 校验；先写 .part 再原子重命名，杜绝半截损坏文件。"""
    expected = expected_sha256.strip()
    if expected == "TBD" and not allow_unpinned:
        raise ValueError(
            f"refusing to download unpinned source ({url}) with sha256='TBD'; pass --allow-unpinned to override"
        )

    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists():
        actual = sha256_file(dest)
        if expected != "TBD":
            if actual != expected:
                raise ValueError(
                    f"existing file {dest} failed sha256 check: expected {expected}, got {actual}"
                )
            return dest, actual
        return dest, actual

    part = dest.with_name(dest.name + ".part")
    req = urllib.request.Request(
        url,
        headers={"User-Agent": "AnIME-DataPipeline/0.1 (+https://github.com/AnIME)"},
    )
    h = hashlib.sha256()
    try:
        with urllib.request.urlopen(req, timeout=timeout_sec) as resp, part.open("wb") as out:
            while True:
                chunk = resp.read(1024 * 1024)
                if not chunk:
                    break
                h.update(chunk)
                out.write(chunk)
        actual = h.hexdigest()
        if expected != "TBD" and actual != expected:
            raise ValueError(
                f"downloaded {url} failed sha256 check: expected {expected}, got {actual}"
            )
        os.replace(part, dest)
        return dest, actual
    except Exception:
        if part.exists():
            part.unlink()
        raise


def unpack_msgpack(data: bytes) -> Any:
    """最小纯标准库 MessagePack 解码器，用于解包 wordfreq .whl 内置 cBpack 词频表。"""
    mv = memoryview(data)
    pos = 0
    total = len(mv)

    def need(n: int) -> None:
        if pos + n > total:
            raise ValueError(f"truncated msgpack stream at offset {pos}, need {n} bytes")

    def read_u8() -> int:
        nonlocal pos
        need(1)
        v = mv[pos]
        pos += 1
        return v

    def read_fmt(fmt: str) -> Any:
        nonlocal pos
        s = struct.Struct(fmt)
        need(s.size)
        vals = s.unpack_from(mv, pos)
        pos += s.size
        return vals[0]

    def read_bytes(n: int) -> bytes:
        nonlocal pos
        need(n)
        out = bytes(mv[pos : pos + n])
        pos += n
        return out

    def decode() -> Any:
        tag = read_u8()
        if tag <= 0x7F:
            return tag
        if tag >= 0xE0:
            return tag - 256
        if 0x80 <= tag <= 0x8F:
            n = tag & 0x0F
            return {decode(): decode() for _ in range(n)}
        if 0x90 <= tag <= 0x9F:
            n = tag & 0x0F
            return [decode() for _ in range(n)]
        if 0xA0 <= tag <= 0xBF:
            n = tag & 0x1F
            return read_bytes(n).decode("utf-8")
        if tag == 0xC0:
            return None
        if tag == 0xC2:
            return False
        if tag == 0xC3:
            return True
        if tag == 0xC4:
            return read_bytes(read_u8())
        if tag == 0xC5:
            return read_bytes(read_fmt(">H"))
        if tag == 0xC6:
            return read_bytes(read_fmt(">I"))
        if tag == 0xCA:
            return read_fmt(">f")
        if tag == 0xCB:
            return read_fmt(">d")
        if tag == 0xCC:
            return read_u8()
        if tag == 0xCD:
            return read_fmt(">H")
        if tag == 0xCE:
            return read_fmt(">I")
        if tag == 0xCF:
            return read_fmt(">Q")
        if tag == 0xD0:
            return read_fmt(">b")
        if tag == 0xD1:
            return read_fmt(">h")
        if tag == 0xD2:
            return read_fmt(">i")
        if tag == 0xD3:
            return read_fmt(">q")
        if tag == 0xD9:
            return read_bytes(read_u8()).decode("utf-8")
        if tag == 0xDA:
            return read_bytes(read_fmt(">H")).decode("utf-8")
        if tag == 0xDB:
            return read_bytes(read_fmt(">I")).decode("utf-8")
        if tag == 0xDC:
            n = read_fmt(">H")
            return [decode() for _ in range(n)]
        if tag == 0xDD:
            n = read_fmt(">I")
            return [decode() for _ in range(n)]
        if tag == 0xDE:
            n = read_fmt(">H")
            return {decode(): decode() for _ in range(n)}
        if tag == 0xDF:
            n = read_fmt(">I")
            return {decode(): decode() for _ in range(n)}
        # ext types（仅跳过载荷避免阻塞）
        if 0xD4 <= tag <= 0xD8:
            ext_sizes = {0xD4: 1, 0xD5: 2, 0xD6: 4, 0xD7: 8, 0xD8: 16}
            _ = read_u8()
            return read_bytes(ext_sizes[tag])
        if tag in (0xC7, 0xC8, 0xC9):
            n = read_u8() if tag == 0xC7 else (read_fmt(">H") if tag == 0xC8 else read_fmt(">I"))
            _ = read_u8()
            return read_bytes(n)
        raise ValueError(f"unsupported msgpack tag 0x{tag:02x}")

    val = decode()
    return val


def read_wordfreq_cbpack(msgpack_bytes: bytes) -> dict[str, float]:
    """解析 wordfreq cBpack（或简化的 {word: zipf} 字典）并返回 {word: zipf}。"""
    obj = unpack_msgpack(msgpack_bytes)
    if isinstance(obj, dict):
        return {str(k): float(v) for k, v in obj.items()}
    if isinstance(obj, list) and obj and isinstance(obj[0], dict) and obj[0].get("format") == "cB":
        out: dict[str, float] = {}
        # wordfreq cBpack 第 idx 个桶对应 p = 10^(-idx/100)，即 Zipf = 9.0 - idx * 0.01
        for idx, bucket in enumerate(obj[1:]):
            if not isinstance(bucket, list):
                continue
            zipf = max(0.0, round(9.0 - idx * 0.01, 4))
            for word in bucket:
                if isinstance(word, str) and word not in out:
                    out[word] = zipf
        return out
    raise ValueError("unrecognized wordfreq msgpack payload structure")


def write_animelex(
    path: Path,
    words: list[LexiconEntry],
    misspellings: list[MisspellingEntry] | None = None,
    flags: int = FLAG_CASE_PRESERVED,
) -> None:
    """写出 ANIMELEX v1 紧凑小端序二进制词典（遵循 .agents/contracts/data-format.md）。"""
    misspell_list = list(misspellings or [])
    sorted_words = sorted(words, key=lambda e: (e.word.lower(), e.word))
    sorted_mis = sorted(misspell_list, key=lambda m: (m.typo, m.target))

    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as fh:
        fh.write(pack_header(MAGIC_LEX, FORMAT_VERSION, flags))
        fh.write(struct.pack("<IIQ", len(sorted_words), len(sorted_mis), 0))
        for entry in sorted_words:
            w_bytes = entry.word.encode("utf-8")
            p_bytes = entry.phonetic.encode("utf-8")
            if len(w_bytes) > 65535:
                raise ValueError(f"word too long: {entry.word!r}")
            if len(p_bytes) > 255:
                raise ValueError(f"phonetic code too long: {entry.phonetic!r}")
            zipf_x100 = max(0, min(65535, int(round(entry.zipf * 100.0))))
            rank = max(0, min(0xFFFFFFFF, int(entry.freq_rank)))
            eflags = entry.entry_flags & 0xFF
            fh.write(struct.pack("<H", len(w_bytes)))
            fh.write(w_bytes)
            fh.write(struct.pack("<HIBB", zipf_x100, rank, eflags, len(p_bytes)))
            if p_bytes:
                fh.write(p_bytes)

        for mis in sorted_mis:
            t_bytes = mis.typo.encode("utf-8")
            g_bytes = mis.target.encode("utf-8")
            if len(t_bytes) > 65535 or len(g_bytes) > 65535:
                raise ValueError("misspelling entry exceeds u16 length")
            weight_x100 = max(0, min(65535, int(round(mis.weight * 100.0))))
            fh.write(struct.pack("<H", len(t_bytes)))
            fh.write(t_bytes)
            fh.write(struct.pack("<H", len(g_bytes)))
            fh.write(g_bytes)
            fh.write(struct.pack("<H", weight_x100))


def read_animelex(path: Path) -> tuple[int, list[LexiconEntry], list[MisspellingEntry]]:
    """安全校验头部并读取 ANIMELEX v1 二进制词典。"""
    data = path.read_bytes()
    _, flags = unpack_header(data, MAGIC_LEX)
    offset = HEADER_STRUCT.size
    counts_struct = struct.Struct("<IIQ")
    if offset + counts_struct.size > len(data):
        raise ValueError("truncated ANIMELEX counts header")
    word_count, mis_count, _ = counts_struct.unpack_from(data, offset)
    offset += counts_struct.size

    words: list[LexiconEntry] = []
    meta_struct = struct.Struct("<HIBB")
    for _ in range(word_count):
        if offset + 2 > len(data):
            raise ValueError("truncated ANIMELEX word length")
        (w_len,) = struct.unpack_from("<H", data, offset)
        offset += 2
        if offset + w_len + meta_struct.size > len(data):
            raise ValueError("truncated ANIMELEX word entry")
        word = data[offset : offset + w_len].decode("utf-8")
        offset += w_len
        zipf_x100, rank, eflags, p_len = meta_struct.unpack_from(data, offset)
        offset += meta_struct.size
        if offset + p_len > len(data):
            raise ValueError("truncated ANIMELEX phonetic field")
        phonetic = data[offset : offset + p_len].decode("utf-8")
        offset += p_len
        words.append(
            LexiconEntry(
                word=word,
                zipf=zipf_x100 / 100.0,
                freq_rank=rank,
                entry_flags=eflags,
                phonetic=phonetic,
            )
        )

    misspellings: list[MisspellingEntry] = []
    for _ in range(mis_count):
        if offset + 2 > len(data):
            raise ValueError("truncated ANIMELEX misspelling typo length")
        (t_len,) = struct.unpack_from("<H", data, offset)
        offset += 2
        if offset + t_len + 2 > len(data):
            raise ValueError("truncated ANIMELEX misspelling typo bytes")
        typo = data[offset : offset + t_len].decode("utf-8")
        offset += t_len
        (g_len,) = struct.unpack_from("<H", data, offset)
        offset += 2
        if offset + g_len + 2 > len(data):
            raise ValueError("truncated ANIMELEX misspelling target bytes")
        target = data[offset : offset + g_len].decode("utf-8")
        offset += g_len
        (weight_x100,) = struct.unpack_from("<H", data, offset)
        offset += 2
        misspellings.append(
            MisspellingEntry(typo=typo, target=target, weight=weight_x100 / 100.0)
        )

    if offset != len(data):
        raise ValueError(f"trailing unexpected bytes in ANIMELEX: {len(data) - offset}")

    return flags, words, misspellings


def write_animengr(
    path: Path,
    vocab: list[str],
    unigrams: list[tuple[int, float, float]],
    bigrams: list[tuple[int, int, float, float]],
    trigrams: list[tuple[int, int, int, float]],
    flags: int = 0,
) -> None:
    """写出 ANIMENGR v1 紧凑小端序 n-gram 二进制语言模型表。"""
    sorted_uni = sorted(unigrams, key=lambda x: x[0])
    sorted_bi = sorted(bigrams, key=lambda x: (x[0], x[1]))
    sorted_tri = sorted(trigrams, key=lambda x: (x[0], x[1], x[2]))

    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as fh:
        fh.write(pack_header(MAGIC_NGR, FORMAT_VERSION, flags))
        fh.write(
            struct.pack(
                "<IIII",
                len(vocab),
                len(sorted_uni),
                len(sorted_bi),
                len(sorted_tri),
            )
        )
        for token in vocab:
            b = token.encode("utf-8")
            if len(b) > 65535:
                raise ValueError(f"vocab token too long: {token!r}")
            fh.write(struct.pack("<H", len(b)))
            fh.write(b)

        uni_struct = struct.Struct("<Iff")
        for wid, lp, bo in sorted_uni:
            fh.write(uni_struct.pack(wid, lp, bo))

        bi_struct = struct.Struct("<IIff")
        for w1, w2, lp, bo in sorted_bi:
            fh.write(bi_struct.pack(w1, w2, lp, bo))

        tri_struct = struct.Struct("<IIIf")
        for w1, w2, w3, lp in sorted_tri:
            fh.write(tri_struct.pack(w1, w2, w3, lp))


def read_animengr(
    path: Path,
) -> tuple[
    int,
    list[str],
    list[tuple[int, float, float]],
    list[tuple[int, int, float, float]],
    list[tuple[int, int, int, float]],
]:
    """安全校验头部并读取 ANIMENGR v1 二进制 n-gram 表。"""
    data = path.read_bytes()
    _, flags = unpack_header(data, MAGIC_NGR)
    offset = HEADER_STRUCT.size
    counts_struct = struct.Struct("<IIII")
    if offset + counts_struct.size > len(data):
        raise ValueError("truncated ANIMENGR counts header")
    v_cnt, u_cnt, b_cnt, t_cnt = counts_struct.unpack_from(data, offset)
    offset += counts_struct.size

    vocab: list[str] = []
    for _ in range(v_cnt):
        if offset + 2 > len(data):
            raise ValueError("truncated ANIMENGR vocab length")
        (t_len,) = struct.unpack_from("<H", data, offset)
        offset += 2
        if offset + t_len > len(data):
            raise ValueError("truncated ANIMENGR vocab token")
        vocab.append(data[offset : offset + t_len].decode("utf-8"))
        offset += t_len

    uni_struct = struct.Struct("<Iff")
    if offset + u_cnt * uni_struct.size > len(data):
        raise ValueError("truncated ANIMENGR unigram table")
    unigrams = []
    for _ in range(u_cnt):
        unigrams.append(uni_struct.unpack_from(data, offset))
        offset += uni_struct.size

    bi_struct = struct.Struct("<IIff")
    if offset + b_cnt * bi_struct.size > len(data):
        raise ValueError("truncated ANIMENGR bigram table")
    bigrams = []
    for _ in range(b_cnt):
        bigrams.append(bi_struct.unpack_from(data, offset))
        offset += bi_struct.size

    tri_struct = struct.Struct("<IIIf")
    if offset + t_cnt * tri_struct.size > len(data):
        raise ValueError("truncated ANIMENGR trigram table")
    trigrams = []
    for _ in range(t_cnt):
        trigrams.append(tri_struct.unpack_from(data, offset))
        offset += tri_struct.size

    if offset != len(data):
        raise ValueError(f"trailing unexpected bytes in ANIMENGR: {len(data) - offset}")

    return flags, vocab, unigrams, bigrams, trigrams
