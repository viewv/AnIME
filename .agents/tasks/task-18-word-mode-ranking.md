---
task_id: "task-18-word-mode-ranking"
worktree: "../wt-anime-word-mode"
allowed_paths:
  - "crates/engine/**"
  - "crates/lexicon/**"
  - "eval/benchmarks/word_ctx_v1.tsv"
  - "eval/review/negative_review.tsv"
  - "scripts/data_pipeline/**"
  - ".agents/tasks/task-18-word-mode-ranking.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
  - "crates/lm-ngram/**"
  - "crates/lm-neural/**"
  - "crates/animed/**"
  - "crates/eval/**"
  - "eval/benchmarks/core_v1.tsv"
  - "eval/benchmarks/core_v2.tsv"
  - "eval/benchmarks/noise_v1.tsv"
  - "eval/benchmarks/noise_v2.tsv"
  - "eval/benchmarks/ood_v1.tsv"
  - "eval/benchmarks/ood_dev_v1.tsv"
  - "eval/benchmarks/ood_academic_v1.tsv"
  - "eval/benchmarks/ood_tm2_v1.tsv"
  - "eval/fixtures/**"
  - "data/MANIFEST.yaml"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-18-word-mode-ranking.md"
---

## Objective (目标)
提升**单词模式**（`space_commits = true`，空格上屏首选）的 Top-1 排序质量，只用 L1 + L2（单词模式 L3 由用户决定暂缓，不得引入）：先建一个模拟"带真实上文逐词上屏"的单词模式评测集 `eval/benchmarks/word_ctx_v1.tsv`，按根因分类失误，再以通用规则（`WORD_PROFILE` 取值 / 单词路径逻辑 / L1 召回）修正，不用词表特判。

## Background
- core_v2（L1+L2）单词级 top1 81.5 vs top5 96.3：单词模式受排序限制。分类 top1：prefix 82.9、typo_simple 92.5、typo_prefix 56.0、typo_phonetic 69.0、context_realword 93.5、exact 99.0。
- 已知召回缺陷：`bateria` → `bacteria`（`baterial` → `bacterial`）从不召回，而 `bacteia` / `bactria` 正常（早位漏字被挡）；`material`（首字母 b→m，非 QWERTY 邻键，且需补全）对 `bateria` 排第一。
- 3 步计划第 2 步（单词模式轨道）；第 3 步整句召回轨道并行，只改 `SENTENCE_PROFILE`。task-16：单 token 路径邻键易混真词须领先 ≥ 3.0。

## 核心子任务
1. 评测先行：`scripts/data_pipeline/build_word_ctx_eval.py` 可复现生成 `word_ctx_v1.tsv`。只用留出句（oasst 留出树 `build_oasst_text.is_heldout_tree`、arXiv 留出 `data/compiled/arxiv_cs/heldout.jsonl`），取句中一个词作为输入、其前的干净词作为上文；词级噪声：前缀截断、邻键、多位置漏字（含早位）、读音、上下文真词混淆；外加干净词反例与 `negative:novel`（词典外技术词）。遵循 `eval-schema.md`（只读；需新标签则写 Blocked），反例经 `eval/review/negative_review.tsv` 分歧裁决。
2. 失误按根因分类。
3. 以通用规则修正：`WORD_PROFILE` 取值、单词路径逻辑或 L1（`crates/lexicon`），不改 `SENTENCE_PROFILE`。
4. 测量：`anime-eval --mode word` 于新集与 core_v2 单词级类别（前后 + Wilson CI）；`--mode sentence` 在 ood_dev_v1 / noise_v2 逐行不变；单键延迟 p50 <= 3 ms、p99 <= 10 ms、max <= 16 ms。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；`crates/engine/**` 仅限单词模式 profile 取值与单词路径逻辑。必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 核心 crate（`protocol` / `lexicon` / `lm-ngram` / `lm-neural` / `engine`）严禁引入 Linux/Fcitx5/macOS 平台专有依赖。
- 禁止无意义流水账注释；保留与键盘物理距离、读音规则、协议不变量、延迟预算相关的 Why 注释。
- 不合并 main、不推送、不重装 / 重启 live 守护进程（`~/.local/bin/animed`）。冻结评测集不得改动。共享 `CARGO_TARGET_DIR=/home/viewv/develop/AnIME/target`。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [DONE — 待评审] 已在 `feat/task-18-word-mode-ranking` 本地提交；未合并 main、未推送、未重装 live 守护进程。
- 评测集：`scripts/data_pipeline/build_word_ctx_eval.py` → `eval/benchmarks/word_ctx_v1.tsv`（2300 行，每行一个词 + 同子句中其前的干净上文）。源句只取留出数据：oasst1 留出树用户提问（`is_heldout_tree`，60%）+ arXiv CS 留出摘要（`heldout.jsonl`，40%），均不进入 L2 训练，因此无需重建 L2。类别：prefix 300（截断前缀非词典词，`||` 列出屈折变体）、typo_simple 500（omit_early / omit / adjacent / insert / transpose 各 100）、typo_prefix 300、typo_phonetic 300（至少一处结构化读音改写 + 一处改写或 a/e/i schwa 互换，DL >= 2）、context_realword 300、exact 600（`negative:clean` 400 + `negative:novel` 200）。标签只用契约已有命名空间（`source:oasst|arxiv`、`negative:clean|novel`），算子 / 位置写在 notes 自由文本，无需改契约。
- 反例复核（分歧裁决）：只裁决被单词模式 L1+L2 改动的反例；`eval/review/negative_review.tsv` 追加 15 条 exclude（源句错拼：enlightnement、pople、inteligence、explaination、metioned、relativly、hawe、ffiles、potentioal、trustworthyness、empasse、Iccan、tively 片段、`you table`、Radioheads）与 5 条 keep（this/has/are 三条干净词误纠属真实误判；heterogenous、archeologist 为变体拼写）。被排除的子句均不在任何冻结集中（已 grep 确认），冻结集再生成不受影响。
- 失误根因（基线）：
  1. **L1 召回**：束搜索按"子树最高词频 - λ·代价"剪枝，不看尚未消费的输入，词首附近漏字在第 2–3 层被挤出束宽（`bateria`、`diersity`、`deonstrated`、`agorithms` 从不召回；词尾同样漏字可召回）。
  2. **新词被改写**：词典外 token 在单 token 路径没有原样候选（只在末位兜底），`ChatGPT -> Chapters`、`LLM -> LM`、`NumPy -> Bumpy`；`negative:novel` 41.5%。
  3. **排序**：输入是合法前缀但非词典词时，高频短词的读音 / 拼写纠错经 L2 PMI 压过补全（`easie -> is`、`affe -> of`、`towa -> to`、`arri -> are`）。
  4. 未修（见遗留）：短词漏字被 L1 截断（`paer`、`usae`、`loing`）；context_realword 11%（保守保留真词，与"不改正确词"冲突）；纯小写新词（`frontend`、`npm`）与错拼无法区分。
- 修复（全部只在 `WORD_PROFILE` 开启，`SENTENCE_PROFILE == BASELINE_PROFILE`；无词表）：
  - L1 `CompactTrie::single_edit_nodes` / `single_edit_search`：沿精确前缀路径逐位穷举漏打 / 多打 / QWERTY 替换 / 换位，只走真实子边（O(26·n)），终点可为完整词或前缀（补全展开取 A* 上界前 16 个）；`QueryOptions::exhaustive_single_edit` 门控（默认 false），与束搜索结果合并。`LexiconProfile.exhaustive_single_edit = true`。
  - `VerbatimProfile.single_token_lead = true`：单 token、非词典词、`token_signals_cased_intent`（与 task-15 整句原样句同一判定，已知驼峰词错拼不领先）时原样以 `Raw` 领先。另有 `single_token_slot: Option<usize>`（默认 None）：把非词典词原样提到第 N 位，novel top3 72.0→100，但 core_v2 typo_prefix top3 82.5→74.0，因此默认关闭，留作可选项。
  - `LexiconProfile.shorter_correction_penalty = 2.0`：输入是合法前缀、本身不是词典词时，比输入短的拼写 / 读音纠错扣 2.0；`phonetic_offset = -0.5`。网格（`--profile`）：penalty 1/2/4 → core_v2 81.1/81.5/81.6；+ph -0.5 → 82.7；+completion 0.3 → 83.0 但 typo_simple -1，未采用；`query_limit` 16/24、`context_weight` 0.5–1.3、`typo_offset -0.3` 均无净收益。
- Changed Files: `crates/lexicon/src/{trie.rs,lib.rs,lexicon.rs}`；`crates/engine/src/{profile.rs,lib.rs}`；`scripts/data_pipeline/{build_word_ctx_eval.py,README.md}`；`eval/benchmarks/word_ctx_v1.tsv`（新）；`eval/review/negative_review.tsv`（追加）；`ROADMAP.md`；本任务单。
- Verification Output:
  - `make check`：通过；`make test`：15 组 `test result: ok`，81 passed、0 failed（新增 `exhaustive_single_edit_recalls_early_omission_beyond_beam`（关闭时同一查询召回不到，开启后召回且代价 = 漏 c 的 deletion_cost，前缀 `bateri` 可补全）、`word_mode_keeps_cased_novel_token_verbatim`（整句模式单 token 不受影响）、`word_mode_prefers_completion_over_shorter_correction_of_valid_prefix`）；`make eval`（smoke）TOTAL 28.6%（同 main）；`make verify-infra`：`INFRA OK`；`python3 -m unittest`（data_pipeline）33 OK；`./scripts/verify-boundaries.sh .agents/tasks/task-18-word-mode-ranking.md`：`BOUNDARY OK（8 个文件，零越界）`，`--base main --tracked-only`：`BOUNDARY OK（10 个文件）`。
  - 资产：`data/compiled/lexicon.animelex.bin`，去泄漏 L2 `~/.local/share/anime/ngram.animengr.bin`，无 `--model`。before = 本分支基线提交（main 4d3e775 代码）重编译，after = 本提交；共享 target 下每次构建前 touch 源码，避免复用其他 worktree 的同指纹产物（曾出现过一次）。
  - **单词模式 Top-1（%，Wilson 95% CI）**，word_ctx_v1（`--mode word`）：

    | 类别 | n | before | after |
    |---|---|---|---|
    | TOTAL | 2300 | 71.6 [69.7, 73.4] | 74.9 [73.1, 76.6] |
    | exact | 600 | 80.0 [76.6, 83.0] | 89.2 [86.4, 91.4] |
    | negative:clean | 400 | 99.2 [97.8, 99.7] | 99.2 [97.8, 99.7] |
    | negative:novel | 200 | 41.5 [34.9, 48.4] | 69.0 [62.3, 75.0] |
    | prefix | 300 | 80.3 [75.5, 84.4] | 82.0 [77.3, 85.9] |
    | typo_simple | 500 | 88.4 [85.3, 90.9] | 89.8 [86.8, 92.2] |
    | typo_simple 漏字 / 词首附近 | 142 | 78.2 [70.7, 84.2] | 81.7 [74.5, 87.2] |
    | typo_prefix | 300 | 62.3 [56.7, 67.6] | 65.3 [59.8, 70.5] |
    | typo_prefix 漏字 / 词首附近 | 91 | 46.2 [36.3, 56.3] | 50.5 [40.5, 60.6] |
    | typo_phonetic | 300 | 88.0 [83.8, 91.2] | 87.7 [83.5, 90.9] |
    | context_realword | 300 | 11.0 [7.9, 15.0] | 11.0 [7.9, 15.0] |

    core_v2（`--mode word`）：

    | 类别 | n | before | after |
    |---|---|---|---|
    | TOTAL | 1375 | 80.7 [78.5, 82.7] | 82.7 [80.6, 84.6] |
    | exact | 200 | 99.0 [96.4, 99.7] | 99.0 [96.4, 99.7] |
    | prefix | 175 | 82.9 [76.6, 87.7] | 90.9 [85.7, 94.3] |
    | typo_simple | 200 | 92.5 [88.0, 95.4] | 93.5 [89.2, 96.2] |
    | typo_prefix | 200 | 56.0 [49.1, 62.7] | 61.0 [54.1, 67.5] |
    | typo_phonetic | 200 | 69.0 [62.3, 75.0] | 70.5 [63.8, 76.4] |
    | context_realword | 200 | 84.5 [78.8, 88.9] | 84.5 [78.8, 88.9] |
    | sentence | 200 | 81.0 [75.0, 85.8] | 80.5 [74.5, 85.4] |

    其他冻结集单词模式（after，仅参考）：noise_v2 46.2、ood_dev_v1 55.2、ood_academic_v1 51.8、ood_tm2_v1 50.3（task-17 基线 45.9 / 51.1 / 46.2 / 50.2）。`bateria -> bacteria` 无上文 Top-1（`the` 上文时 L2 偏向 material，第 5 位）；`easie -> easier` Top-1。
  - **整句模式不变**：core_v2 / noise_v2 / ood_dev_v1 / ood_academic_v1 / ood_tm2_v1 的 `--mode sentence --dump-rows`（1375 / 1650 / 1030 / 1030 / 600 行，每行 rank + top-5 全文）before 与 after **逐字节相同**；top1 83.1 / 78.7 / 66.9 / 59.3 / 67.2。
  - **单键延迟**（TOTAL key p50 / p99 / max µs）：单词模式 after：word_ctx_v1 50 / 562 / 959（before 42 / 463 / 727），core_v2 27 / 493 / 712，其余冻结集 p99 <= 410、max <= 1317——全部远低于 3 / 10 / 16 ms。整句模式（代码路径未改、逐行相同）在本机负载 3.6（其他会话并行构建 / 评测）下 p50 <= 0.5 ms、p99 <= 5.8 ms 均达标，但 max 在 before 与 after 两个二进制上都偶发 > 16 ms（ood_dev_v1 before 20.6 / 18.7 ms，after 18.2 / 20.6 ms；noise_v2 before 17.5、after 17.0），属单次系统抖动（task-17 在 main 上也记录过 23.6 ms），不是本任务引入；正式门禁建议在空闲机器上复测整句模式 max。
- 遗留 / 后续：
  1. 纯小写新词（`frontend`、`roleplay`、`npm`）仍被改写：L2 词表能证明这些是真实用词，但 `NgramModel` 未暴露 unigram 查询，需要 `crates/lm-ngram` 新接口（跨 crate 任务）；或启用 `verbatim.single_token_slot`（top3 换 top3）。
  2. 短词漏字（`paer -> paper`、`usae`、`loing`）：L1 截断前由词频主导，需按编辑类型调代价或召回后再截断，会同时影响整句模式，宜单独任务。
  3. context_realword 11%：单词模式保守保留真词，与用户"不改正确词"的偏好一致，未调。
  4. `exhaustive_single_edit` 对整句模式召回可能同样有益，由整句召回轨道自行评估（只需改 `SENTENCE_PROFILE`）。
- **补丁（合并 main fe85569 之后，L3 续写帧丢原样领先）**：
  - 现象：live 守护进程 `enable_neural=true` + 单词模式敲 `ChatGPT`，rev 0 = [ChatGPT(Raw), …]，异步 L3 帧 rev 1 = [Chat, Chapters, Chapter, …]，原样领先消失。根因：`apply_neural_rerank` 丢弃所有 `consumed_chars == total_chars` 的 `CandidateKind::Raw`（原意是丢末位兜底再由 `ensure_raw_and_limit` 重建），而 `place_single_token_verbatim` 的领先候选也是 `Raw`。单词模式下纠错 L3 对单 token 返回空重排，但 animed 每帧都调用 `apply_neural_rerank`（含仅续写帧）。
  - 修复（`crates/engine/src/lib.rs`）：原样候选以 `comment = "verbatim"`（`VERBATIM_COMMENT` / `is_verbatim_raw`）标记，`apply_neural_rerank` 只丢弃未标记的 Raw 兜底；原样候选与其他主候选一样按分数参与合并（不按位置特判）——只有续写的帧不改主候选分数，它保持首位；若 L3 真的给出重排分数，也按分数正常竞争。
  - 单测 `verbatim_lead_survives_async_l3_continuation_frame`：单词模式 `ChatGPT` rev 0 首位为 verbatim Raw，喂入仅续写帧后 rev 1 仍首位、续写 `is great` 出现、`ChatGPT` 不重复；纯小写错拼 `chapterz` 的末位 Raw 兜底仍按原规则重建。**去掉修复后该测试失败**（已验证），加上后通过。
  - 端到端（`animed --stdio --features vulkan --model anime-l3-360m-v2.Q4_K_M.gguf`，独立 `--data-dir`，二进制由 task-07 分支 67b0ed3 本地合并本补丁构建，未合并到任何分支）：上文 `I think `、`space_commits=true`、`enable_neural=true` 敲 `ChatGPT`——补丁前 rev 1 = [Chat, Chapters, Chapter, Chart]（复现），补丁后 rev 1 = [ChatGPT(Raw), Chat, Chapters, Chapter]。
  - 评测（`--mode word`）：L1+L2 下 word_ctx_v1 / core_v2 逐行 dump 与 task-18 最终版**逐字节相同**（TOTAL 74.9 / 82.7）。`--model anime-l3-360m-v2.Q4_K_M.gguf`（Vulkan，task-07 本地合并构建）：补丁前后逐字节相同，且与 L1+L2 结果逐字节相同（word_ctx_v1 TOTAL 74.9 / top3 87.7 / negative:novel 69.0；core_v2 TOTAL 82.7 / top3 90.6）；L3 调用 2139 / 199 次、p50 0 µs——纠错 L3 对单 token 直接返回空重排。单键延迟 p99 <= 0.62 ms；一次 max 14.8 ms（core_v2 补丁前二进制，sentence 类），均 <= 16 ms。
  - **评测口径缺口（未修，在本任务路径外）**：`anime-eval --model` 只在重排结果非空时调用 `apply_neural_rerank`，且从不传入续写，因此无法复现 animed 的"仅续写帧"——本缺陷在离线评测中不可见。建议在 task-07 分支的 `crates/eval/src/metrics.rs` 中改为与 animed 相同：每次都调用 `apply_neural_rerank(seq, &reranked, &continuations)`（续写取 `predict_continuations`）。
  - 门禁：`make check` 通过；`make test` 15 组 82 passed、0 failed；`make eval` TOTAL 28.6%；`make verify-infra` `INFRA OK`；`verify-boundaries --base main --tracked-only` `BOUNDARY OK`。task-07 本地合并状态下 `cargo test -p anime-engine -p anime-lm-neural -p animed` 40 passed、0 failed。

