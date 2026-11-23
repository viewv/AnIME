#!/usr/bin/env python3
"""AnIME 开源数据管线离线单元测试套件（零外部依赖，使用临时目录）。"""

from __future__ import annotations

import gzip
from collections import Counter
import io
import json
import random
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

import build_eval_benchmark  # noqa: E402
import build_eval_v2  # noqa: E402
import build_eval_v3  # noqa: E402
import build_l3_sft  # noqa: E402
import replay_local_log  # noqa: E402
import build_ngram_kn  # noqa: E402
import build_oasst_text  # noqa: E402
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
    deterministic_split_bucket,
    in_split,
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

    def test_deterministic_split_zero_overlap(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            csv_path = root / "typos.csv"
            lines = ["typo,correction"]
            for idx in range(60):
                a = chr(97 + idx // 26)
                b = chr(97 + idx % 26)
                lines.append(f"typow{a}{b}x,apple")
            csv_path.write_text("\n".join(lines), encoding="utf-8")

            train_tsv = root / "train.tsv"
            eval_tsv = root / "eval.tsv"
            build_misspellings.main(["--typos-csv", str(csv_path), "--split", "train", "--out-tsv", str(train_tsv)])
            build_misspellings.main(["--typos-csv", str(csv_path), "--split", "eval", "--out-tsv", str(eval_tsv)])

            train_typos = {
                r.split("\t")[0]
                for r in train_tsv.read_text(encoding="utf-8").splitlines()
                if r.strip() and not r.startswith("#")
            }
            eval_typos = {
                r.split("\t")[0]
                for r in eval_tsv.read_text(encoding="utf-8").splitlines()
                if r.strip() and not r.startswith("#")
            }
            self.assertGreater(len(train_typos), 30)
            self.assertGreater(len(eval_typos), 5)
            self.assertEqual(train_typos & eval_typos, set())
            self.assertEqual(deterministic_split_bucket("apple"), deterministic_split_bucket("apple"))
            self.assertTrue(in_split("apple", "train", 20) ^ in_split("apple", "eval", 20))

    def test_eval_benchmark_open_and_local_adapters(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            lex_tsv = root / "lexicon.tsv"
            lex_words = [
                ("the", 7.7),
                ("to", 7.4),
                ("and", 7.3),
                ("from", 6.8),
                ("form", 5.8),
                ("go", 6.2),
                ("here", 6.1),
                ("want", 5.9),
                ("find", 5.8),
                ("hotel", 5.5),
                ("near", 5.4),
                ("apple", 5.2),
                ("algorithm", 4.9),
                ("beautiful", 5.1),
                ("environment", 5.0),
                ("kubernetes", 4.4),
                ("configuration", 4.7),
                ("definitely", 5.3),
                ("receive", 5.0),
                ("pneumonia", 3.9),
                ("psychology", 4.6),
                ("document", 5.1),
                ("check", 5.6),
                ("this", 6.7),
                ("please", 5.9),
                ("I", 7.5),
                ("I'm", 6.4),
                ("don't", 6.6),
            ]
            lex_lines = ["# word\tzipf\tfreq_rank\tentry_flags\tsource"]
            for i, (w, z) in enumerate(lex_words, 1):
                lex_lines.append(f"{w}\t{z:.2f}\t{i}\t4\tscowl+wordfreq")
            lex_tsv.write_text("\n".join(lex_lines), encoding="utf-8")

            typos_csv = root / "typos.csv"
            typos_csv.write_text(
                "\n".join(
                    [
                        "typo,correction",
                        "teh,the",
                        "appie,apple",
                        "definately,definitely",
                        "numonia,pneumonia",
                        "sikology,psychology",
                    ]
                ),
                encoding="utf-8",
            )

            # 合成 GitHub Typo Corpus jsonl.gz
            gh_gz = root / "gh.jsonl.gz"
            gh_records = [
                {
                    "commit": "c1",
                    "edits": [
                        {
                            "src": {"text": "check this dokument please", "lang": "eng"},
                            "tgt": {"text": "check this document please", "lang": "eng"},
                            "prob_typo": 0.95,
                            "is_typo": True,
                        },
                        {
                            "src": {"text": "go form here", "lang": "eng"},
                            "tgt": {"text": "go from here", "lang": "eng"},
                            "prob_typo": 0.92,
                            "is_typo": True,
                        },
                    ],
                }
            ]
            with gzip.open(gh_gz, "wt", encoding="utf-8") as fh:
                for rec in gh_records:
                    fh.write(json.dumps(rec) + "\n")

            # 合成 MultiWOZ 2.2 dialogues_001.json
            mw_dir = root / "mw"
            mw_dir.mkdir()
            (mw_dir / "dialogues_001.json").write_text(
                json.dumps(
                    [
                        {
                            "dialogue_id": "d1",
                            "turns": [
                                {"speaker": "USER", "utterance": "I want to find a hotel from here"},
                                {"speaker": "SYSTEM", "utterance": "Sure"},
                                {"speaker": "USER", "utterance": "I'm sure I don't want this document"},
                            ],
                        }
                    ]
                ),
                encoding="utf-8",
            )

            out_open = root / "eval_open.tsv"
            stats_json = root / "eval_stats.json"
            rc = build_eval_benchmark.main(
                [
                    "--lexicon-tsv",
                    str(lex_tsv),
                    "--typos-csv",
                    str(typos_csv),
                    "--github-typo-jsonl",
                    str(gh_gz),
                    "--multiwoz-dir",
                    str(mw_dir),
                    "--per-category",
                    "10",
                    "--split",
                    "all",
                    "--out-tsv",
                    str(out_open),
                    "--stats-json",
                    str(stats_json),
                ]
            )
            self.assertEqual(rc, 0)
            stats = json.loads(stats_json.read_text(encoding="utf-8"))
            self.assertGreater(stats["total"], 10)
            self.assertGreater(stats["categories"]["exact"], 0)
            self.assertGreater(stats["categories"]["prefix"], 0)
            self.assertGreater(stats["categories"]["context_realword"], 0)
            self.assertGreater(stats["categories"]["sentence"], 0)

            for raw in out_open.read_text(encoding="utf-8").splitlines():
                if not raw.strip() or raw.startswith("#"):
                    continue
                parts = raw.split("\t")
                self.assertEqual(len(parts), 5)
                self.assertTrue(build_eval_benchmark.is_valid_sample(parts[0], parts[1], parts[2], parts[3]))

            # 本机非商用适配器测试（Birkbeck + JFLEG）
            birkbeck = root / "missp.dat"
            birkbeck.write_text("$pneumonia\nnumonia\n$apple\nappie\n", encoding="utf-8")
            jfleg_src = root / "test.src"
            jfleg_ref = root / "test.ref0"
            jfleg_src.write_text("i want go to teh school\n", encoding="utf-8")
            jfleg_ref.write_text("I want to go to the school\n", encoding="utf-8")
            local_out = root / "eval_local.tsv"
            rc_loc = build_eval_benchmark.main(
                [
                    "--local-birkbeck",
                    str(birkbeck),
                    "--local-jfleg-src",
                    str(jfleg_src),
                    "--local-jfleg-ref",
                    str(jfleg_ref),
                    "--local-out",
                    str(local_out),
                ]
            )
            self.assertEqual(rc_loc, 0)
            loc_rows = [
                r.split("\t")
                for r in local_out.read_text(encoding="utf-8").splitlines()
                if r.strip() and not r.startswith("#")
            ]
            self.assertEqual(len(loc_rows), 3)
            self.assertEqual({r[0] for r in loc_rows}, {"typo_phonetic", "typo_simple", "sentence"})


class TestEvalV2Builder(unittest.TestCase):
    LEX = {
        w: (w, z)
        for w, z in [
            ("grenade", 3.5), ("grenades", 3.0), ("this", 7.0), ("thing", 6.0),
            ("calculate", 3.8), ("calculated", 3.6), ("calculating", 3.2), ("want", 6.0),
            ("to", 7.4), ("go", 6.5), ("home", 5.8), ("i", 7.0), ("that", 7.0), ("hat", 4.5),
        ]
    }

    def test_inflection_variants_require_real_stem(self):
        self.assertEqual(build_eval_v2.inflection_variants("grenade", self.LEX), ["grenades"])
        self.assertEqual(
            build_eval_v2.inflection_variants("calculated", self.LEX), ["calculate", "calculating"]
        )
        # this 去 s 得到的 "thi" 不是词，不能展开出 thing
        self.assertEqual(build_eval_v2.inflection_variants("this", self.LEX), [])

    def test_derive_core_v2_drops_word_prefix_and_adds_refs(self):
        rows = [
            ("prefix", "", "this", "things", "word input"),
            ("prefix", "", "calcul", "calculate", "x"),
            ("typo_prefix", "", "grehad", "grenades", "y"),
            ("sentence", "", "i wnt", "I want", "z"),
        ]
        out, stats = build_eval_v2.derive_core_v2(rows, self.LEX)
        self.assertEqual(stats["prefix_dropped_input_is_word"], 1)
        self.assertEqual(out[0][3], "calculate || calculated || calculating")
        self.assertEqual(out[1][3], "grenades || grenade")
        self.assertEqual(out[2], rows[3])

    def test_noise_operators_inject_exactly_the_named_error(self):
        rng = random.Random(0)
        toks = ["I", "want", "to", "go", "home"]
        self.assertEqual(build_eval_v2.noise_boundary_gemination(toks, rng, self.LEX), "i want o go home")
        self.assertEqual(build_eval_v2.noise_space_missing(["want", "to"], rng, self.LEX), "wantto")
        shifted = build_eval_v2.noise_space_shift(["want", "home"], rng, self.LEX)
        self.assertIn(shifted, {"wan thome", "wanth ome"})
        dropped = build_eval_v2.noise_leading_drop(["want", "to"], rng, self.LEX)
        self.assertIn(dropped, {"ant to", "want o"})
        self.assertEqual(build_eval_v2.noise_contraction(["I'm", "here"], rng, self.LEX), "im here")
        self.assertIsNone(build_eval_v2.noise_contraction(["no", "apostrophe"], rng, self.LEX))
        self.assertIn("hat", build_eval_v2.single_edits("that"))

    def test_realword_sample_is_context_realword_with_tag(self):
        rng = random.Random(1)
        row = build_eval_v2.realword_edit_sample(["I", "think", "that", "works"], rng, {"hat"})
        self.assertEqual(row, ("context_realword", "I think ", "hat works", "that works",
                               "noise:realword_edit that->hat"))


class TestNgramKnOrchestrator(unittest.TestCase):
    def test_collects_context_and_every_reference_for_leak_filter(self):
        with tempfile.TemporaryDirectory() as td:
            tsv = Path(td) / "e.tsv"
            tsv.write_text(
                "# comment\n"
                "typo_prefix\t\tgrenad\tgrenade || grenades\tx\n"
                "context_realword\tgo \tform here\tfrom here\tnoise:realword_edit\n",
                encoding="utf-8",
            )
            texts = build_ngram_kn.collect_eval_texts([tsv])
            self.assertEqual(texts, ["grenade", "grenades", "go", "from here"])
            out = Path(td) / "ex.txt"
            self.assertEqual(build_ngram_kn.write_exclusion_file(texts + ["go"], out), 4)

    def test_extracts_every_dialogue_turn_one_per_line(self):
        with tempfile.TemporaryDirectory() as td:
            js = Path(td) / "dialogues_001.json"
            js.write_text(json.dumps([
                {"turns": [{"speaker": "USER", "utterance": " I need  a taxi. "},
                           {"speaker": "SYSTEM", "utterance": "Where to?"},
                           {"speaker": "USER", "utterance": ""}]},
            ]), encoding="utf-8")
            out = Path(td) / "turns.txt"
            self.assertEqual(build_ngram_kn.extract_dialogue_text([js], out), 2)
            self.assertEqual(out.read_text(encoding="utf-8"), "I need a taxi.\nWhere to?\n")


class TestEvalV3Builder(unittest.TestCase):
    def test_review_file_excludes_only_exclude_decisions(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "review.tsv"
            p.write_text("# header\nThanks You A lot\texclude\tsrc error\nfine here\tkeep\tengine\n", encoding="utf-8")
            self.assertEqual(build_eval_v3.load_review(p), {"thanks you a lot"})
            self.assertEqual(build_eval_v3.load_review(Path(td) / "missing.tsv"), set())

    def test_multi_noise_applies_at_least_two_errors(self):
        lex = {w: (w, 5.0) for w in "i want to go home now please".split()}
        toks = "i want to go home now please".split()
        for seed in range(20):
            out = build_eval_v3.noise_multi(toks, random.Random(seed), lex)
            if out is None:
                continue
            self.assertNotEqual(out, " ".join(toks))
            self.assertEqual(out, out.lower())

    def test_librispeech_holdout_is_deterministic_and_full_scan(self):
        lex = {w: (w, 5.0) for w in "the cat sat on a mat dog ran far away".split()}
        lines = ["THE CAT SAT ON A MAT", "THE DOG RAN FAR AWAY", "A CAT RAN", "ZZZ QQQ WWW EEE"] * 50
        lines += [f"THE CAT SAT ON A MAT {'DOG ' * (i % 3)}".strip() for i in range(300)]
        with tempfile.TemporaryDirectory() as td:
            gz = Path(td) / "c.txt.gz"
            with gzip.open(gz, "wt", encoding="utf-8") as f:
                f.write("\n".join(lines) + "\n")
            a = build_eval_v3.librispeech_holdout_clauses(gz, lex, bucket=100_000)
            b = build_eval_v3.librispeech_holdout_clauses(gz, lex, bucket=100_000)
            self.assertEqual(a, b)
            # bucket=100000 取全部；非词典词句与 < 4 词句被过滤，重复句去重
            self.assertIn(["the", "cat", "sat", "on", "a", "mat"], a)
            self.assertTrue(all(len(c) >= 4 for c in a))
            self.assertEqual(len(a), len({" ".join(c) for c in a}))
            self.assertEqual(build_eval_v3.librispeech_holdout_clauses(gz, lex, bucket=0), [])


class TestReplayLocalLog(unittest.TestCase):
    def test_convert_categorizes_and_skips_malformed(self):
        rows, skipped = replay_local_log.convert([
            "# comment",
            "\ti want o go\tI want to go",
            "\tbeautif\tbeautiful",
            "\tteh\tthe",
            "Hello \tapple\tapple",
            "only two\tcolumns",
            "\t\tempty typed",
        ])
        cats = [r.split("\t")[0] for r in rows]
        self.assertEqual(cats, ["sentence", "prefix", "typo_simple", "exact"])
        self.assertTrue(all(r.endswith("\tsource:local") for r in rows))
        self.assertEqual(skipped, 2)

    def test_refuses_to_write_under_eval(self):
        with tempfile.TemporaryDirectory() as td:
            log = Path(td) / "log.tsv"
            log.write_text("\tteh\tthe\n", encoding="utf-8")
            rc = replay_local_log.main([str(log), "--out", str(REPO_ROOT / "eval/local.tsv"), "--convert-only"])
            self.assertEqual(rc, 2)
            self.assertFalse((REPO_ROOT / "eval/local.tsv").exists())


class TestOasstText(unittest.TestCase):
    def test_clean_markdown_strips_code_links_and_marks(self):
        text = (
            "## How to **fix** it\n"
            "Run `pip install x` then see [the docs](https://example.com/a).\n"
            "```python\nprint('hi')\n```\n"
            "| a | b |\n"
            "- First step: visit https://foo.bar now\n"
            "1. Second _step_"
        )
        self.assertEqual(
            build_oasst_text.clean_markdown(text),
            ["How to fix it", "Run then see the docs.", "First step: visit now", "Second step"],
        )

    def test_keep_message_filters_language_review_and_flags(self):
        ok = {"lang": "en", "deleted": False, "synthetic": False, "review_result": True, "role": "prompter"}
        self.assertTrue(build_oasst_text.keep_message(ok))
        for k, v in [("lang", "es"), ("deleted", True), ("synthetic", True), ("review_result", False), ("role", "x")]:
            self.assertFalse(build_oasst_text.keep_message({**ok, k: v}))

    def test_convert_writes_role_files(self):
        msgs = [
            {"lang": "en", "deleted": False, "synthetic": False, "review_result": True, "role": "prompter", "text": "hi there"},
            {"lang": "en", "deleted": False, "synthetic": False, "review_result": True, "role": "assistant", "text": "```x```"},
            {"lang": "de", "deleted": False, "synthetic": False, "review_result": True, "role": "assistant", "text": "hallo"},
        ]
        with tempfile.TemporaryDirectory() as td:
            stats = build_oasst_text.convert((json.dumps(m) for m in msgs), Path(td))
            self.assertEqual((Path(td) / "oasst1_prompter.txt").read_text(encoding="utf-8"), "hi there\n")
            self.assertEqual((Path(td) / "oasst1_assistant.txt").read_text(encoding="utf-8"), "")
            self.assertEqual((stats["prompter_words"], stats["messages"]), (2, 3))


class TestOodDevBuilder(unittest.TestCase):
    LEX = {w: (w, 5.0) for w in "how can i use this image in a the you we write code for".split()}

    def test_heldout_split_is_by_tree_and_deterministic(self):
        ids = [f"tree-{i}" for i in range(2000)]
        held = [t for t in ids if build_oasst_text.is_heldout_tree(t)]
        self.assertEqual(held, [t for t in ids if build_oasst_text.is_heldout_tree(t)])
        self.assertTrue(0.10 < len(held) / len(ids) < 0.20)

    def test_convert_skips_heldout_trees(self):
        held = next(f"t{i}" for i in range(1000) if build_oasst_text.is_heldout_tree(f"t{i}"))
        kept = next(f"t{i}" for i in range(1000) if not build_oasst_text.is_heldout_tree(f"t{i}"))
        base = {"lang": "en", "deleted": False, "synthetic": False, "review_result": True, "role": "prompter"}
        msgs = [{**base, "message_tree_id": held, "text": "secret eval question"},
                {**base, "message_tree_id": kept, "text": "training question"}]
        with tempfile.TemporaryDirectory() as td:
            stats = build_oasst_text.convert((json.dumps(m) for m in msgs), Path(td))
            self.assertEqual((Path(td) / "oasst1_prompter.txt").read_text(encoding="utf-8"), "training question\n")
            self.assertEqual(stats["heldout_skipped"], 1)

    def test_dev_clauses_split_standard_and_novel(self):
        std, novel = build_eval_v3.dev_clauses(
            ["How can I use this image in numpy? Write the code for you.", "How can I use this image in a"], self.LEX
        )
        self.assertIn(["How", "can", "I", "use", "this", "image", "in", "a"], std)
        self.assertIn(["How", "can", "I", "use", "this", "image", "in", "numpy"], novel)
        self.assertTrue(all("numpy" not in c for c in std))
        rows = build_eval_v3.build_novel_negatives(novel, 5, set(), "t")
        self.assertTrue(rows and all(r[4] == "negative:novel" and r[2] == r[3] for r in rows))
        self.assertEqual(build_eval_v3.build_novel_negatives(novel, 5, {" ".join(novel[0]).lower()}, "t"), rows[1:])


class TestL3Sft(unittest.TestCase):
    def test_arxiv_units_chain_context_and_cap(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "train.jsonl"
            p.write_text(json.dumps({"id": "2501.00001", "cats": ["cs.CL"],
                                     "abstract": "We propose a new method. It improves recall on hard queries."}) + "\n")
            units = list(build_l3_sft.arxiv_units(p, 10))
            self.assertEqual([u[3] for u in units], ["We propose a new method", "It improves recall on hard queries"])
            self.assertEqual(units[0][2], "")
            self.assertEqual(units[1][2], "We propose a new method")
            self.assertTrue(all(u[0] == "arxiv:2501.00001" and u[1] == "arxiv" for u in units))
            self.assertEqual(len(list(build_l3_sft.arxiv_units(p, 1))), 1)

    def test_refuses_arxiv_heldout_file(self):
        with tempfile.TemporaryDirectory() as td:
            ho = Path(td) / "heldout.jsonl"
            ho.write_text("")
            self.assertEqual(build_l3_sft.main(["--arxiv", str(ho), "--out-dir", str(Path(td) / "o")]), 1)

    def test_template_matches_rust_golden(self):
        # 与 crates/lm-neural/src/prompt.rs::correction_template_matches_training_golden 逐字节一致
        self.assertEqual(build_l3_sft.build_correction_prompt("  Can you  help me ", "i want o  go"),
                         "<|ctx|> Can you help me\n<|typed|> i want o go\n<|fix|>")
        self.assertEqual(build_l3_sft.correction_target(" I want to go "), " I want to go<|end|>")
        self.assertEqual(build_l3_sft.build_correction_prompt("", "hello"), "<|ctx|> \n<|typed|> hello\n<|fix|>")
        long_ctx = "word " * 60 + "finalword"
        trimmed = build_l3_sft._trim_context_tail(long_ctx, 160)
        self.assertLessEqual(len(trimmed), 160)
        self.assertTrue(trimmed.endswith("finalword") and not trimmed.startswith(" "))

    def test_leak_filter_uses_ngram_build_normalization(self):
        lf = build_l3_sft.LeakFilter(["I'd like to book a table for two"])
        self.assertTrue(lf.is_leak("i'd LIKE to book a table for two"))
        self.assertTrue(lf.is_leak("Hello. like to book a table tonight"))  # 共享 5-gram
        self.assertFalse(lf.is_leak("book a table"))
        self.assertEqual(build_l3_sft.normalize_sentences("It's 2 PM, ok?"), [["it's"], ["pm"], ["ok"]])

    def test_clauses_and_examples(self):
        self.assertEqual(build_l3_sft.clauses("Hi there! How do I install numpy, please?"),
                         [["Hi", "there"], ["How", "do", "I", "install", "numpy"]])
        lex = {w: (w, 5.0) for w in "how do i install the package please want to go home".split()}
        rng = random.Random(3)
        typed, target, tag = build_l3_sft.make_example("", "How do I install numpy", rng, lex, set(), {}, 0.0, 0.0)
        self.assertEqual((target, tag), ("How do I install numpy", "identity_novel"))
        self.assertIn(typed, {"How do I install numpy", "how do i install numpy"})
        seen = Counter()
        for seed in range(60):
            ex = build_l3_sft.make_example("", "i want to go home please", random.Random(seed), lex, set(), {}, 0.3, 0.2)
            self.assertIsNotNone(ex)
            seen[ex[2].split("_")[0]] += 1
            if not ex[2].startswith("identity"):
                self.assertNotEqual(ex[0], ex[1])
        self.assertTrue(seen["identity"] > 5 and sum(v for k, v in seen.items() if k != "identity") > 20)


if __name__ == "__main__":
    unittest.main()
