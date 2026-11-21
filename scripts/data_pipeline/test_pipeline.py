#!/usr/bin/env python3
"""AnIME 开源数据管线离线单元测试套件（零外部依赖，使用临时目录）。"""

from __future__ import annotations

import gzip
import io
import json
import struct
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import build_lexicon  # noqa: E402
import build_misspellings  # noqa: E402
import build_ngram  # noqa: E402
import fetch_sources  # noqa: E402
from common import (  # noqa: E402
    FLAG_CASE_PRESERVED,
    LexiconEntry,
    MAGIC_LEX,
    MAGIC_NGR,
    MisspellingEntry,
    load_and_validate_manifest,
    pack_header,
    read_animelex,
    read_animengr,
    read_wordfreq_cbpack,
    unpack_header,
    unpack_msgpack,
    validate_license_string,
    write_animelex,
    write_animengr,
)


def pack_msgpack(obj: object) -> bytes:
    """仅用于测试生成标准 MessagePack 载荷。"""
    if obj is None:
        return b"\xc0"
    if obj is False:
        return b"\xc2"
    if obj is True:
        return b"\xc3"
    if isinstance(obj, int):
        if 0 <= obj <= 0x7F:
            return bytes([obj])
        if obj < 0 and obj >= -32:
            return bytes([obj & 0xFF])
        return b"\xd3" + struct.pack(">q", obj)
    if isinstance(obj, float):
        return b"\xcb" + struct.pack(">d", obj)
    if isinstance(obj, str):
        raw = obj.encode("utf-8")
        if len(raw) < 32:
            return bytes([0xA0 | len(raw)]) + raw
        if len(raw) < 256:
            return b"\xd9" + bytes([len(raw)]) + raw
        return b"\xda" + struct.pack(">H", len(raw)) + raw
    if isinstance(obj, list):
        if len(obj) < 16:
            hdr = bytes([0x90 | len(obj)])
        else:
            hdr = b"\xdc" + struct.pack(">H", len(obj))
        return hdr + b"".join(pack_msgpack(item) for item in obj)
    if isinstance(obj, dict):
        if len(obj) < 16:
            hdr = bytes([0x80 | len(obj)])
        else:
            hdr = b"\xde" + struct.pack(">H", len(obj))
        return hdr + b"".join(pack_msgpack(k) + pack_msgpack(v) for k, v in obj.items())
    raise TypeError(f"unsupported test msgpack type: {type(obj)}")


class TestManifestAndLicenses(unittest.TestCase):
    def test_repo_manifest_is_valid(self) -> None:
        sources = load_and_validate_manifest(REPO_ROOT / "data" / "MANIFEST.yaml", REPO_ROOT)
        self.assertGreaterEqual(len(sources), 4)
        ids = {s["id"] for s in sources}
        self.assertIn("scowl", ids)
        self.assertIn("wordfreq-en", ids)
        self.assertIn("ngram-corpus", ids)

    def test_fetch_sources_verify_only(self) -> None:
        rc = fetch_sources.main(["--verify-only"])
        self.assertEqual(rc, 0)

    def test_license_whitelist_rejects_noncommercial_and_gpl(self) -> None:
        validate_license_string("MIT / BSD-like")
        validate_license_string("CC-BY-SA-4.0")
        validate_license_string("Public Domain")
        validate_license_string("MPL-2.0")
        for bad in (
            "CC-BY-NC-SA-3.0",
            "GPL-3.0",
            "AGPL-3.0",
            "Proprietary Commercial",
            "Unknown-Custom",
        ):
            with self.assertRaises(ValueError, msg=f"should reject {bad}"):
                validate_license_string(bad)

    def test_ready_source_with_tbd_sha256_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            mpath = Path(tmp) / "MANIFEST.yaml"
            mpath.write_text(
                "\n".join(
                    [
                        "version: 1",
                        "sources:",
                        "  - id: bad-ready",
                        "    status: ready",
                        '    description: "test"',
                        '    source_url: "https://example.org/data.tar.gz"',
                        '    version: "1.0"',
                        '    license: "MIT"',
                        '    sha256: "TBD"',
                        '    build_script: "scripts/data_pipeline/build_lexicon.py"',
                        '    notes: "should fail"',
                    ]
                ),
                encoding="utf-8",
            )
            with self.assertRaises(ValueError):
                load_and_validate_manifest(mpath, REPO_ROOT)


class TestBinaryFormatsAndMsgpack(unittest.TestCase):
    def test_header_validation(self) -> None:
        hdr = pack_header(MAGIC_LEX, 1, FLAG_CASE_PRESERVED)
        self.assertEqual(len(hdr), 16)
        ver, flags = unpack_header(hdr, MAGIC_LEX)
        self.assertEqual(ver, 1)
        self.assertEqual(flags, FLAG_CASE_PRESERVED)

        with self.assertRaises(ValueError):
            unpack_header(b"BADMAGIC" + hdr[8:], MAGIC_LEX)
        with self.assertRaises(ValueError):
            unpack_header(MAGIC_LEX + struct.pack("<II", 2, 0), MAGIC_LEX)
        with self.assertRaises(ValueError):
            unpack_header(b"short", MAGIC_NGR)

    def test_animelex_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "lex.bin"
            words = [
                LexiconEntry("apple", 5.21, 1, 0b100, "APL"),
                LexiconEntry("I", 7.12, 1, 0b101, "A"),
                LexiconEntry("don't", 6.45, 2, 0b110, "TNT"),
            ]
            mis = [MisspellingEntry("appie", "apple", 0.95)]
            write_animelex(out, words, mis, flags=FLAG_CASE_PRESERVED)

            flags, got_words, got_mis = read_animelex(out)
            self.assertEqual(flags, FLAG_CASE_PRESERVED)
            self.assertEqual(len(got_words), 3)
            self.assertEqual({w.word for w in got_words}, {"apple", "I", "don't"})
            self.assertEqual(got_mis, mis)

            # 截断校验
            raw = out.read_bytes()
            trunc = Path(tmp) / "trunc.bin"
            trunc.write_bytes(raw[:-2])
            with self.assertRaises(ValueError):
                read_animelex(trunc)

    def test_animengr_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "ngr.bin"
            vocab = ["<unk>", "<bos>", "<eos>", "apple", "eat"]
            uni = [(0, -4.0, -0.4), (1, -1.0, -0.4), (2, -1.0, -0.4), (3, -1.5, -0.4), (4, -1.6, -0.4)]
            bi = [(1, 4, -0.5, -0.4), (4, 3, -0.3, -0.4)]
            tri = [(1, 4, 3, -0.2)]
            write_animengr(out, vocab, uni, bi, tri)

            flags, got_v, got_u, got_b, got_t = read_animengr(out)
            self.assertEqual(flags, 0)
            self.assertEqual(got_v, vocab)
            self.assertEqual(len(got_u), 5)
            self.assertEqual(len(got_b), 2)
            self.assertEqual(len(got_t), 1)
            self.assertAlmostEqual(got_t[0][3], -0.2, places=5)

    def test_msgpack_and_cbpack_decode(self) -> None:
        buckets: list[object] = [{"format": "cB", "version": 1}]
        for _ in range(130):
            buckets.append([])
        buckets.append(["the"])  # idx = 130 -> Zipf = 7.70
        for _ in range(219):
            buckets.append([])
        buckets.append(["kubernetes", "apple"])  # idx = 350 -> Zipf = 5.50

        encoded = pack_msgpack(buckets)
        decoded = unpack_msgpack(encoded)
        self.assertIsInstance(decoded, list)
        freqs = read_wordfreq_cbpack(encoded)
        self.assertAlmostEqual(freqs["the"], 7.70, places=2)
        self.assertAlmostEqual(freqs["kubernetes"], 5.50, places=2)
        self.assertAlmostEqual(freqs["apple"], 5.50, places=2)


class TestEndToEndBuilders(unittest.TestCase):
    def test_misspellings_and_lexicon_pipeline(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # 1. 构造合成 Wikipedia For_machines 与 crate-ci/typos CSV
            wiki = root / "wiki.txt"
            wiki.write_text(
                "<pre>\ndefinately->definitely\nappie->apple, apply\nbad word->ignore\n</pre>\n",
                encoding="utf-8",
            )
            typos_csv = root / "typos.csv"
            typos_csv.write_text(
                "typo,correction\nteh,the\nrecieve,receive\nnonsense,notaword\n",
                encoding="utf-8",
            )

            # 2. 构造合成 SCOWL tar.gz
            scowl_tar = root / "scowl.tar.gz"
            with tarfile.open(scowl_tar, "w:gz") as tf:
                files = {
                    "scowl-2020.12.07/final/english-words.10": b"the\napple\nreceive\ndefinitely\nI\n",
                    "scowl-2020.12.07/final/english-contractions.10": b"I'm\ndon't\n",
                    "scowl-2020.12.07/final/english-words.95": b"obscureword\n",
                }
                for name, data in files.items():
                    ti = tarfile.TarInfo(name=name)
                    ti.size = len(data)
                    tf.addfile(ti, io.BytesIO(data))

            # 3. 构造合成 wordfreq .whl
            buckets: list[object] = [{"format": "cB", "version": 1}]
            for _ in range(130):
                buckets.append([])
            buckets.append(["the", "i"])  # zipf 7.70
            for _ in range(250):
                buckets.append([])
            buckets.append(["apple", "definitely", "receive", "kubernetes", "don't"])  # zipf 5.19
            gz_bytes = gzip.compress(pack_msgpack(buckets))

            whl_path = root / "wordfreq-3.1.1-py3-none-any.whl"
            with zipfile.ZipFile(whl_path, "w") as zf:
                zf.writestr("wordfreq/data/large_en.msgpack.gz", gz_bytes)

            # 4. 先构词表 TSV 以便用于 misspellings 白名单过滤，再合并生成最终 ANIMELEX
            lex_tsv = root / "lexicon.tsv"
            lex_bin = root / "lexicon.animelex.bin"
            rc_lex1 = build_lexicon.main(
                [
                    "--scowl-tar",
                    str(scowl_tar),
                    "--max-scowl-level",
                    "70",
                    "--wordfreq-whl",
                    str(whl_path),
                    "--modern-min-zipf",
                    "3.0",
                    "--out-tsv",
                    str(lex_tsv),
                    "--out-bin",
                    str(lex_bin),
                ]
            )
            self.assertEqual(rc_lex1, 0)

            mis_tsv = root / "misspellings.tsv"
            rc_mis = build_misspellings.main(
                [
                    "--wiki-raw",
                    str(wiki),
                    "--typos-csv",
                    str(typos_csv),
                    "--lexicon-tsv",
                    str(lex_tsv),
                    "--out-tsv",
                    str(mis_tsv),
                ]
            )
            self.assertEqual(rc_mis, 0)

            rc_lex2 = build_lexicon.main(
                [
                    "--scowl-tar",
                    str(scowl_tar),
                    "--max-scowl-level",
                    "70",
                    "--wordfreq-whl",
                    str(whl_path),
                    "--modern-min-zipf",
                    "3.0",
                    "--misspellings-tsv",
                    str(mis_tsv),
                    "--out-tsv",
                    str(lex_tsv),
                    "--out-bin",
                    str(lex_bin),
                ]
            )
            self.assertEqual(rc_lex2, 0)

            _, words, mis = read_animelex(lex_bin)
            word_set = {w.word for w in words}
            self.assertIn("I", word_set)
            self.assertIn("I'm", word_set)
            self.assertIn("don't", word_set)
            self.assertIn("kubernetes", word_set)
            self.assertNotIn("obscureword", word_set)

            mis_pairs = {(m.typo, m.target) for m in mis}
            self.assertIn(("teh", "the"), mis_pairs)
            self.assertIn(("definately", "definitely"), mis_pairs)
            self.assertIn(("recieve", "receive"), mis_pairs)
            self.assertNotIn(("nonsense", "notaword"), mis_pairs)

    def test_ngram_corpus_and_arpa_pipelines(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            corpus_gz = root / "corpus.txt.gz"
            with gzip.open(corpus_gz, "wt", encoding="utf-8") as fh:
                for _ in range(4):
                    fh.write("I want to eat an apple.\n")
                    fh.write("We go from here to there.\n")

            out_bin = root / "ngram.animengr.bin"
            stats_json = root / "stats.json"
            rc = build_ngram.main(
                [
                    "--corpus",
                    str(corpus_gz),
                    "--min-unigram",
                    "2",
                    "--min-bigram",
                    "2",
                    "--min-trigram",
                    "3",
                    "--out-bin",
                    str(out_bin),
                    "--stats-json",
                    str(stats_json),
                ]
            )
            self.assertEqual(rc, 0)
            stats = json.loads(stats_json.read_text(encoding="utf-8"))
            self.assertGreater(stats["bigram_count"], 5)
            self.assertGreater(stats["trigram_count"], 3)

            _, vocab, uni, bi, tri = read_animengr(out_bin)
            self.assertEqual(vocab[:3], ["<unk>", "<bos>", "<eos>"])
            self.assertIn("apple", vocab)
            self.assertEqual(len(uni), stats["unigram_count"])
            self.assertEqual(len(bi), stats["bigram_count"])
            self.assertEqual(len(tri), stats["trigram_count"])

            # ARPA 模式测试
            arpa_gz = root / "lm.arpa.gz"
            arpa_text = (
                "\\data\\\n"
                "ngram 1=4\n"
                "ngram 2=2\n"
                "ngram 3=1\n\n"
                "\\1-grams:\n"
                "-1.0\t<s>\t-0.3\n"
                "-1.2\t</s>\t0.0\n"
                "-1.5\tgo\t-0.4\n"
                "-1.4\tfrom\t-0.4\n\n"
                "\\2-grams:\n"
                "-0.5\tgo from\t-0.3\n"
                "-0.6\t<s> go\t-0.3\n\n"
                "\\3-grams:\n"
                "-0.2\t<s> go from\n\n"
                "\\end\\\n"
            )
            with gzip.open(arpa_gz, "wt", encoding="utf-8") as fh:
                fh.write(arpa_text)

            arpa_bin = root / "arpa.animengr.bin"
            rc_arpa = build_ngram.main(["--arpa", str(arpa_gz), "--out-bin", str(arpa_bin)])
            self.assertEqual(rc_arpa, 0)
            _, av, au, ab, at = read_animengr(arpa_bin)
            self.assertIn("from", av)
            self.assertEqual(len(au), 5)  # <unk>, <bos>, <eos>, from, go
            self.assertEqual(len(ab), 2)
            self.assertEqual(len(at), 1)


if __name__ == "__main__":
    unittest.main()
