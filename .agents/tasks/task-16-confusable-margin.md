---
task_id: "task-16-confusable-margin"
worktree: "../wt-anime-confusable"
allowed_paths:
  - "crates/engine/**"
  - ".agents/tasks/task-16-confusable-margin.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
  - "crates/lm-neural/**"
  - "crates/animed/**"
  - "crates/eval/**"
  - "eval/**"
  - "scripts/**"
  - "data/MANIFEST.yaml"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-16-confusable-margin.md"
---

## Objective (目标)
单词模式（`SpaceCommits=True`）下，有上文时用户正确敲出的常见词不得被上下文易混真词轻易翻盘：修正 `crates/engine/src/confusables.rs::expand_slot_confusables` 的先验分差，使 `type fast` / `to type fast` 中 `fast` 稳居 Top-1，同时 `fill out the from -> form` 一类真词纠错（core_v2 `context_realword`）不回退。

## Background
- live 守护进程复现：输入 `fast`，上文 `type ` 时 `fast` -0.63 vs `vast` -1.34；上文 `to type ` 时 0.45 vs -0.15；无上文时 2.87 vs -4.99（无上文不展开易混词）。用户实际看到 `vast` 排第一。
- 机制：`has_prev_word_ctx` 时 `lexicon.contextual_confusables` 加入同音 / 近音 / 换位 / 邻键真词，分数 `anchor - effective_penalty`（anchor = 精确词 L1 分 = log_freq + 6.5）。易混词比原词罕见时 `deficit_relief` 把惩罚降到下限 0.88；随后 L2 `score_in_context` 的 bigram 加分对两者几乎无区分（`type fast` / `type vast` 都稀有）。
- `ConfusableCandidate` 只有 `word` / `log_freq` / `relative_penalty`，无来源标记（`crates/lexicon` 本任务只读）。
- 单词模式不走 L3（task-07 纠错模型跳过单 token），因此只评测 L1+L2。

## 核心子任务
1. 用证据定位翻盘原因（逐行看 `context_realword` 命中所需的最小分差），选择修复：按易混族类型区分下限，和 / 或要求易混词拿到明确的 L2 优势后才可超过精确词。
2. 单测：`fast` 在上文 `type ` / `to type ` 后保持 Top-1；`fill out the from -> form` 类测试继续通过。
3. 评测：`anime-eval --group-by-tag`（词典 `data/compiled/lexicon.animelex.bin`，去泄漏 L2 `~/.local/share/anime/ngram.animengr.bin`，无 `--model`）在 `core_v2`（`context_realword` 不得超出 CI 回退，`exact` / `typo_*` 同理）、`noise_v2`、`ood_dev_v1`、`ood_academic_v1`、`ood_tm2_v1`（`negative:clean` 不得下降）前后对比 + Wilson 95% CI；另用 scratch TSV（不入库）测"上文 + 精确常见词 + 邻键真词近邻"行。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 核心 crate（`protocol` / `lexicon` / `lm-ngram` / `lm-neural` / `engine`）严禁引入 Linux/Fcitx5/macOS 平台专有依赖。
- 不得重新安装或重启 live 守护进程（`~/.local/bin/animed`，systemd --user）除非用户明确同意；不合并 main、不推送。
- 冻结评测集不得改动。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
