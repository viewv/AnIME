# AnIME Open-Source Data Pipeline

本目录存放用于校验、下载、清洗与编译 100% 开源洁净英文词库、错拼集及 2/3-gram 语料的**可复现构建脚本**（仅依赖 Python 3 标准库 + `pyyaml`）。

## 约束与合规

1. 所有外部数据源必须登记在 [data/MANIFEST.yaml](../../data/MANIFEST.yaml)（来源 URL、版本、开源协议、SHA256、构建脚本路径）。
2. `common.py` 强制执行开源协议白名单（仅允许 `MIT`、`Apache-2.0`、`BSD-*`、`MPL-2.0`、`ISC`、`CC0-1.0`、`Public Domain`、`CC-BY-4.0`、`CC-BY-SA-4.0`，显式拒绝 `-NC` / `GPL` / `AGPL` / `Proprietary` 等）。
3. 原始下载与编译生成的二进制产物默认输出至 `data/raw/` 与 `data/compiled/`（均在 `.gitignore` 中，禁止提交进 Git）。

## 标准构建流程

```bash
# 1. 离线校验 data/MANIFEST.yaml 协议与字段合法性
python3 scripts/data_pipeline/fetch_sources.py --verify-only

# 2. 在联网环境按 SHA256 拉取已锁定数据源到 data/raw/
python3 scripts/data_pipeline/fetch_sources.py --all
# 若需拉取 Wikipedia For_machines（sha256=TBD）并获取实际哈希进行版本锁定：
# python3 scripts/data_pipeline/fetch_sources.py --source wikipedia-common-misspellings --allow-unpinned

# 3. 先构建基础词表（SCOWL 白名单 + wordfreq wheel cBpack 纯标准库直读）
python3 scripts/data_pipeline/build_lexicon.py \
  --scowl-tar data/raw/scowl/scowl-2020.12.07.tar.gz \
  --wordfreq-whl data/raw/wordfreq-en/wordfreq-3.1.1-py3-none-any.whl

# 4. 清洗合并错拼表（可选过滤至合法词库白名单）并回写进 ANIMELEX
python3 scripts/data_pipeline/build_misspellings.py \
  --wiki-raw data/raw/wikipedia-common-misspellings/wikipedia_for_machines.txt \
  --lexicon-tsv data/compiled/lexicon.tsv
python3 scripts/data_pipeline/build_lexicon.py \
  --scowl-tar data/raw/scowl/scowl-2020.12.07.tar.gz \
  --wordfreq-whl data/raw/wordfreq-en/wordfreq-3.1.1-py3-none-any.whl \
  --misspellings-tsv data/compiled/misspellings.tsv

# 5. 流式编译 2/3-gram 语言模型（支持 .txt/.txt.gz 语料或 .arpa/.arpa.gz）
python3 scripts/data_pipeline/build_ngram.py \
  --corpus data/raw/ngram-corpus/librispeech-lm-norm.txt.gz \
  --max-lines 1000000
```

## 离线单元测试

```bash
python3 -m unittest discover -s scripts/data_pipeline -p 'test_*.py' -v
```

## 二进制格式说明（供 Task 02 / Task 03 读取器对齐）

所有文件严格遵循 [.agents/contracts/data-format.md](../../.agents/contracts/data-format.md) §3 的 **16 字节 Little-Endian 固定头**：

```text
0x00..0x08  [u8; 8]  Magic: b"ANIMELEX" 或 b"ANIMENGR"
0x08..0x0C  u32 LE   Format version (= 1)
0x0C..0x10  u32 LE   Feature flags (bit0: case_preserved, bit1: phonetic_index, bit2: compact_quantized)
```

### `ANIMELEX` v1 载荷布局

- 头部之后紧接 `<IIQ`（16 字节）：`word_count: u32`、`misspelling_count: u32`、`reserved: u64 = 0`。
- `word_count` 条词典项（按 `(word.lower(), word)` 升序）：
  - `word_len: u16 LE` + `word: [u8; word_len]` (UTF-8)
  - `zipf_x100: u16 LE`、`freq_rank: u32 LE`、`entry_flags: u8`（bit0 大写/专名，bit1 含撇号缩写，bit2 来自 SCOWL）、`phonetic_len: u8` + `phonetic: [u8; phonetic_len]`
- `misspelling_count` 条常见人类错拼直达项（按 `(typo, target)` 升序）：
  - `typo_len: u16 LE` + `typo`、`target_len: u16 LE` + `target`、`weight_x100: u16 LE`

### `ANIMENGR` v1 载荷布局（词表全小写规范化，查询侧按 `to_ascii_lowercase()` 对齐）

- 头部之后紧接 `<IIII`（16 字节）：`vocab_count`、`unigram_count`、`bigram_count`、`trigram_count`。
- 词表区（`vocab_count` 项，0=`<unk>`、1=`<bos>`、2=`<eos>`，其后按字典序排列）：`len: u16 LE` + `token_utf8`。
- Unigram 表（按 `word_id` 升序）：`<Iff`（`word_id: u32`, `log10_prob: f32`, `backoff: f32`）。
- Bigram 表（按 `(w1, w2)` 升序）：`<IIff`（`w1: u32`, `w2: u32`, `log10_prob: f32`, `backoff: f32`）。
- Trigram 表（按 `(w1, w2, w3)` 升序）：`<IIIf`（`w1: u32`, `w2: u32`, `w3: u32`, `log10_prob: f32`）。
