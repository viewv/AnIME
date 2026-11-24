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
- Status: [IN PROGRESS — 用量上限中断，WIP 已提交；尚未跑完整门禁]
- 已完成：
  - `scripts/data_pipeline/build_word_ctx_eval.py` + `eval/benchmarks/word_ctx_v1.tsv`（2300 行：prefix 300、typo_simple 500（omit_early / omit / adjacent / insert / transpose 各 100）、typo_prefix 300、typo_phonetic 300、context_realword 300、negative:clean 400、negative:novel 200；oasst 留出 60% / arXiv 留出 40%）。标签只用已有命名空间（`source:*`、`negative:*`），无需改契约。
  - L1：`CompactTrie::single_edit_nodes` / `single_edit_search` 穷举单次编辑，`QueryOptions::exhaustive_single_edit` 门控；`LexiconProfile.exhaustive_single_edit`，仅 `WORD_PROFILE = true`（整句模式不变）。`bateria -> bacteria` 无上文时 Top-1（有 `the` 上文时 L2 偏向 material，排第 5）。
  - `cargo test -p anime-lexicon -p anime-engine` 通过。
- 基线（--mode word，L1+L2，de-leaked ngram）word_ctx_v1 top1：TOTAL 71.4、exact 79.3（negative:clean 99.2、negative:novel 39.5）、prefix 80.3、typo_simple 88.4（omit early 78.2）、typo_prefix 62.3（omit early 46.2）、typo_phonetic 88.0、context_realword 11.0。core_v2 word TOTAL 80.7。
- 穷举单编辑后：word_ctx typo_simple 89.0、typo_prefix 63.3、TOTAL 71.7；core_v2 不变 80.7；key p99 约 0.5→0.7 ms。
- 待办（按收益）：
  1. **反例复核**：negative:novel 中约 15 行是源句错拼（enlightnement、pople、inteligence、explaination、metioned、relativly、hawe 等），须写入 `eval/review/negative_review.tsv` 并重新生成；negative:clean 3 行失误待裁决。
  2. **新词保护**：单词模式单 token 路径对 `token_signals_cased_intent` 成立的 token 让原样领先（ChatGPT、LLM、JSON、NumPy 约占 novel 失误一半）；小写新词可把原样放到第 2 位（需权衡 typo top3）。
  3. **排序**：短的高频读音 / 纠错候选压过合法前缀补全（easie->is、affe->of、towa->to）。网格（--profile）：phonetic_offset=-0.5 → core_v2 82.0 / word_ctx 72.0；completion_offset=0.5 → typo_prefix +2.7 / +6.0；context_weight 1.3 → context_realword 19.0 但 prefix -2.6。query_limit 16/24 无益。
  4. 短词漏字仍召回不到（paer、usae、loing、appes）：L1 截断（opts.limit）前词频主导，需按编辑类型调整代价或扩大召回后再截断。
  5. 收尾：`--mode sentence` 在 ood_dev_v1 / noise_v2 逐行 diff、延迟门禁、Wilson CI（脚本见会话笔记）、`make check && make test && make eval && make verify-infra && verify-boundaries`、ROADMAP。
- Changed Files: `crates/lexicon/src/{trie.rs,lib.rs,lexicon.rs}`、`crates/engine/src/{profile.rs,lib.rs}`、`scripts/data_pipeline/build_word_ctx_eval.py`、`eval/benchmarks/word_ctx_v1.tsv`、本任务单。
- Verification Output: 仅 lexicon / engine 单测通过；完整门禁未跑。
