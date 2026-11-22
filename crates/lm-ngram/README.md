# L2 统计语言模型、词格解码与 Task 03 端到端实测报告（`crates/lm-ngram`）

`crates/lm-ngram`（`anime-lm-ngram`）是 AnIME 三层混合架构的 **L2 统计解码与本机学习内核**。它并非需要 GPU 训练的神经网络，而是由开源文本语料流式统计编译出的紧凑 2/3-gram Katz Backoff 概率表、二阶（3-gram）Top-K Beam Viterbi 词格解码器，以及本机 SQLite `user.db` WAL 个人词频/搭配学习存储共同组成，协同 [crates/engine](../engine/src/lib.rs) 在同步按键预算（`p99 <= 5 ms`）内完成多词整句纠错、上下文易混真词消歧与本机越用越准能力。

- **平台纯净度**：零外部 crates.io 依赖，零 Linux/macOS 系统调用与环境变量读取，Workspace 级 `unsafe_code = "forbid"`。
- **契约兼容性**：完整实现 [`NgramModel`](src/lib.rs) Trait 与 [`NoopNgramModel`](src/lib.rs)，并严格遵循 [data-format.md](../../.agents/contracts/data-format.md) 的 `ANIMENGR` v1 二进制布局与 `user.db` Schema。

---

## 一、L1 + L2 协同架构与数据流

```text
预编辑串 preedit (如 "i wnt to eat an appie") + 光标上文 context.before
  │
  ├──▶ 按空格切分词槽 slots[0..M]
  │      ├── 单槽（M=1）：EngineSession token_lex_cache -> L1 候选
  │      │     ├── 无前文：直接叠加 UserStore.word_boost()，保持 L1 独立词频基准零偏移
  │      │     ├── 句末标点/换行后：触发首字母自动大写（如 "Hello. " + "this" -> "This"）
  │      │     └── 有前文实词：confusables.rs 展开易混搭档（-1.35 先验）+ 3-gram PMI lift + user_bigrams
  │      │
  │      └── 多槽（M>1）：L1 词槽（命中组合内缓存）+ confusables/简写展开
  │            ▼
  │      [L2 二阶 Beam Viterbi 解码器 (BEAM_WIDTH=24)]
  │        step = LEX_LAMBDA * rel_lex + LM_LAMBDA * log10_trigram(p2, p1, w) + user_store.bigram_boost
  │            ▼
  │      Top-K 整句候选 (CandidateKind::Sentence) + 首词部分选词 + 兜底 Raw
  │
  └──▶ 用户选词上屏 / Enter 原样确认（当 privacy == false 且 delete_before == 0）
         ├── 立即更新内存 RwLock 热表（O(1) 无锁盘延迟）
         ├── 异步批量事务写入 SQLite WAL `user.db`（user_words + user_bigrams）
         └── 同步学习新词至 L1 Layer 3（LayeredLexicon::learn_word），下次拼错/前缀即刻联想
```

---

## 二、核心子模块设计与关键数学设计

| 模块文件 | 核心职责 | 关键算法与工程权衡 |
|---|---|---|
| [src/format.rs](src/format.rs) | `ANIMENGR` v1 编解码 | 严格对齐 [common.py](../../scripts/data_pipeline/common.py) 16B LE 头与 `<Iff` / `<IIff` / `<IIIf` 表结构，校验词表边界、有限浮点与尾部零残留；同时提供编码器便于闭环单测。 |
| [src/model.rs](src/model.rs) | Katz Backoff 2/3-gram 查表 | 1. 按排列有序 `Vec` 二分查找（1-gram O(1)，2/3-gram O(log N)）；<br>2. **OOV 下界保护**：不把未登录词映射到聚集海量长尾总质量的高概率 `<unk>`（`-2.12`），而是赋予严格低于真实长尾词的 `oov_word_log10_p`（`-7.14`），杜绝 `ive`/`theyre`/`tomorrow's` 等噪声反超真词；<br>3. **高频前缀未见二元封顶**：对 `bi_out_degree >= 4` 的前缀，未见二元回退概率不超过其最小已观测二元 `min_bi_lp - 0.15`，防止 `the from` 仅凭高 unigram 压过语料已见的 `the form`。 |
| [src/viterbi.rs](src/viterbi.rs) | Top-K 二阶 Beam Viterbi | `BEAM_WIDTH=24`、每词槽最多 `10` 候选、同一 `(prev2, prev1)` 状态最多保留 `2` 条最优路径兼顾 Top-K 多样性；对 L1 得分按 `slot_max` 归一化（`LEX_LAMBDA=0.55`, `LM_LAMBDA=1.25`），保证强语境能翻转易混词而不误伤罕见精确词。 |
| [src/user_db.rs](src/user_db.rs) | SQLite `user.db` WAL 学习库 | 读路径全走内存 `RwLock`；写路径经后台 `sqlite3` WAL 持久进程批量合并 `BEGIN IMMEDIATE ... ON CONFLICT DO UPDATE ... COMMIT` 提交；`privacy == true` 时内存与磁盘均零记录。 |
| [../engine/src/confusables.rs](../engine/src/confusables.rs) | 语境易混组与短信简写 | 仅在多词或具上文时补充成对易混真词（`their`/`there`/`they're`、`form`/`from`、`your`/`you're`、`its`/`it's` 等，`-1.35` 相对先验）与高频简写（`hw`/`r`/`u`/`yu`/`ur`）。 |
| [../engine/src/lib.rs](../engine/src/lib.rs) | 组合内 L1 词槽缓存与状态机 | `EngineSession::token_lex_cache` 缓存单次组合中未变化前序词的 L1 候选，使多词整句末键输入主要只查当前正输入词，守住 `< 1 ms` 同步延迟。 |

---

## 三、真实 OpenSLR 11 编译与 Task 03 端到端 7 类实测报告

### 1. 静态模型构建统计（100% Public Domain 语料）

- **数据来源**：`data/MANIFEST.yaml` 已登记源 `ngram-corpus`（OpenSLR 11 `librispeech-lm-norm.txt.gz`，Public Domain）
- **构建命令**：流式扫描前 `1,000,000` 行（`999,999` 句 / `22,229,141` 词次），阈值 `min_unigram=4, min_bigram=4, min_trigram=6, max_vocab=65535`
- **产物规格**：`data/compiled/ngram.animengr.bin`（**16 MB**）：`vocab=65,535`、`1-gram=65,535`、`2-gram=537,173`、`3-gram=368,160`
- **Rust 加载耗时（release）**：L1（120k 词）**205 ms** + L2（16 MB `ANIMENGR`）**31 ms**

### 2. Tier 2 核心基准：[eval/benchmarks/core_v1.tsv](../../eval/benchmarks/core_v1.tsv)（全量 7 大类 1,400 条样本）

测试入口：[crates/engine/tests/real_l1_l2_eval.rs](../engine/tests/real_l1_l2_eval.rs)（`cargo test --release -p anime-engine --test real_l1_l2_eval -- --nocapture`）。

| 场景类别 | 样本数 | Base L1 + L2 (Top-1 / 3 / 5) | 3-Layer L1 + L2 (Top-1 / 3 / 5) | 末键延迟 p50 / p99 | Phase 1 / Phase 2 门禁对比 |
|---|---:|---|---|---|---|
| `exact` | 200 | 99.0% / 99.0% / 99.5% | **100.0%** / 100.0% / 100.0% | 11 us / 22 us | 通过 Phase 1 (`>=98%`) 与 Phase 2 (`>=99%`) |
| `prefix` | 200 | 68.0% / 96.0% / 99.5% | 68.5% / **96.5%** / **100.0%** | 83 us / 162 us | — |
| `typo_simple` | 200 | **92.5%** / **100.0%** / 100.0% | **92.5%** / **100.0%** / 100.0% | 366 us / 457 us | 通过 Phase 1 (`>=85%`) 与 Phase 2 (`>=92%`) |
| `typo_prefix` | 200 | 39.0% / 69.0% / 79.5% | 39.5% / 69.5% / 80.0% | 358 us / 440 us | — |
| `typo_phonetic` | 200 | 69.5% / **85.5%** / 91.5% | 69.5% / **85.5%** / 91.5% | 357 us / 474 us | 通过 Phase 1 (`Top3 >=75%`) 与 Phase 2 (`Top3 >=85%`) |
| `context_realword` | 200 | **65.0%** / **91.5%** / 91.5% | **65.0%** / **91.5%** / 91.5% | 54 us / 155 us | 91.5% 已进入 Top-3；L3 将推高 Top-1 (`>=85%`) |
| `sentence` | 200 | **74.5%** / **88.5%** / 91.5% | **75.5%** / **89.5%** / **92.5%** | 564 us / 1196 us | 修复 `<unk>` 倒挂后由 55.5% 跃升至 75.5% |
| **全量末键同步延迟** | **1,400** | p50 **291 us** / p99 **966 us** | p50 **297 us** / p99 **973 us** | **`< 1 ms`** | 远优于 Phase 1 (`p50<=2 ms, p99<=5 ms`) |

### 3. Tier 3 万级开源真实基准：`data/compiled/eval_large_open.tsv`（全量 7 大类 9,888 条样本，Base L1 + L2）

| 场景类别 | 样本数 | Top-1 | Top-3 | Top-5 | Strict Top-1 | 末键 p50 / p99 | Phase 1 门禁 |
|---|---:|---:|---:|---:|---:|---|---|
| `exact` | 1,500 | **99.8%** | 99.9% | 99.9% | 99.8% | 10 us / 19 us | PASS (`>= 98%`) |
| `prefix` | 1,500 | 58.3% | **95.6%** | **99.2%** | 58.3% | 79 us / 149 us | — |
| `typo_simple` | 1,500 | **93.2%** | **98.4%** | **99.0%** | 93.2% | 345 us / 490 us | PASS (`>= 85%`) |
| `typo_prefix` | 1,500 | 27.3% | 65.1% | 73.8% | 27.3% | 341 us / 462 us | — |
| `typo_phonetic` | 1,500 | 60.0% | **83.6%** | 89.8% | 59.8% | 346 us / 562 us | PASS (`Top3 >= 75%`) |
| `context_realword` | 888 | **64.4%** | **94.4%** | **94.6%** | 64.4% | 49 us / 139 us | 94.4% Top-3 高召回底座 |
| `sentence` | 1,500 | **60.6%** | **77.4%** | **80.5%** | 49.7% | 565 us / 1268 us | 修复 `<unk>` 倒挂后由 45.4% 提升至 60.6% |
| **全量末键同步延迟** | **9,888** | — | — | — | — | **309 us / 1009 us** | 满足 `p50<=2 ms, p99<=5 ms` 硬门禁 |

---

## 四、效果评估与 Phase 2（L3）衔接空间

### 已验证效果
1. **同步 L1+L2 延迟稳定在 ~1 ms 级**：得益于 `CompactTrie` 剪枝与 `EngineSession::token_lex_cache`，9,888 条真实输入（含 1,500 条整句）的末键同步延迟仅 `p50 = 309 us`、`p99 = 1009 us`（Phase 1 预算为 `p50 <= 2 ms, p99 <= 5 ms`）。
2. **`<unk>` OOV 下界与未见二元封顶修复带来显著跃升**：修复 `<unk>` 质量（`-2.12`）导致的概率倒挂后，`core_v1` 整句 Top-1 从 `55.5%` 提升到 `74.5%`（Layered 达 **75.5%** / Top-3 **89.5%**），万级 `eval_large_open` 整句 Top-1 从 `45.4%` 提升到 `60.6%`（Top-3 **77.4%**）。
3. **L2 已稳稳托住 Top-3 召回**：`context_realword` 在 `core_v1` 与万级评测集分别达到 **91.5%** 与 **94.4%** Top-3，为 Phase 2 的 L3 神经重排留足高品质候选池。

### 客观局限与后续演进方向
1. **语料域偏差（19 世纪文学书库 vs 现代口语/技术对话）**：OpenSLR 11 源自 Gutenberg 古典文学，如 `"over their heads"`、`let's` vs 复数 `lets`、`I hope you're` 等现代短句频次偏低；后续可在数据管线补充现代维基/开源技术对话文本或更高行数切分重编 `ngram.animengr.bin`。
2. **句法与长距离语义（留待 Phase 2 L3 端侧小 Transformer）**：例如 `"a very lose/loose screw"`、`"a positive affect/effect"`、`"the school principle/principal"` 以及碰撞缩写与常见实义词的句首歧义（`"ill need"` vs `"I'll need"`），超出 2 词窗口统计边界，将在 Phase 2 通过异步 `seq`/`revision` 神经精排完成从 Top-3 到 Top-1 >= 85% 的最终跃迁。

---

## 五、复现与校验命令

```bash
# 1. 下载 OpenSLR 11（如尚未缓存）并流式编译 ANIMENGR 二进制（100 万行）
python3 scripts/data_pipeline/fetch_sources.py --source ngram-corpus
python3 scripts/data_pipeline/build_ngram.py \
  --corpus data/raw/ngram-corpus/librispeech-lm-norm.txt.gz \
  --max-lines 1000000 --min-unigram 4 --min-bigram 4 --min-trigram 6 \
  --max-vocab 65535 --out-bin data/compiled/ngram.animengr.bin

# 2. 运行 crates/lm-ngram 单元测试与 L1+L2 全量 7 类 release 基准门禁
cargo test -p anime-lm-ngram
cargo test --release -p anime-engine --test real_l1_l2_eval -- --nocapture

# 3. 检查全仓构建、Task 03 边界与契约门禁
cargo check --workspace
./scripts/verify-boundaries.sh .agents/tasks/task-03-ngram-and-engine.md
```
