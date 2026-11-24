---
task_id: "task-19-sentence-recall-v2"
worktree: "/home/viewv/develop/wt-anime-sentence-recall-v2"
allowed_paths:
  - "crates/engine/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
  - ".agents/tasks/task-19-sentence-recall-v2.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
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
acceptance_cmd: "make check && make test && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-19-sentence-recall-v2.md --base main"
---

## Objective (目标)
修复整句模式（`space_commits = false`）召回：当前正确整句经常根本不在候选里，L3 纠错模型（task-07 v2）无从选择。在不改单词模式行为、不破坏单键延迟契约的前提下，提升 ood_dev / ood_academic / ood_tm2 / noise_v2 的 top5 召回。

## Background
- 用户批准的 3 步计划第 3 步（整句召回轨道），与第 2 步单词模式轨道（task-18）并行；脚手架为 task-17（`crates/engine/src/profile.rs` `ModeProfile`）。
- **只改 `SENTENCE_PROFILE` 与整句 / 词格路径**：`WORD_PROFILE` 不动；L1（`crates/lexicon`）与 `crates/lm-ngram` 的改动若被单词模式共用，必须由整句 profile 字段开关、默认值保持单词模式逐行不变（`--mode word --dump-rows` 前后 `cmp` 一致）。
- 起点（L3 = `anime-l3-360m-v2.Q4_K_M.gguf`，去泄漏 L2 `~/.local/share/anime/ngram.animengr.bin`，词库 `data/compiled/lexicon.animelex.bin`），top1 / top5 / 召回缺口：ood_dev 83.8 / 86.7 / 13.3，ood_academic 75.8 / 80.1 / 19.9，ood_tm2 74.3 / 81.7 / 18.3，noise_v2 84.2 / 87.3 / 12.7。ood_dev 按标签召回缺口：noise:multi 55.0、boundary_gemination 30.0、space_extra 30.0、leading_drop 23.3、space_missing 16.7、realword_edit 15.0、contraction 10.0、space_shift 8.3。

## 核心子任务
1. 导出全部未召回行（`anime-eval --mode sentence --group-by-tag --show-failures N --format json`，`ci_rank` 为 None）。
2. 根因归类：每槽候选上限、词格 beam / n-best、空格编辑搜索（`segment_runon`、`SPACE_EDIT_PENALTY`）、漏首字母预算、2–3 处叠加错误、撇号 / 缩写。
3. 在整句 profile 或 L1 / 词格中做通用修复（不针对评测样本打补丁）。
4. 测量前后 top1 / top5（L1+L2 与 +L3 v2 各一套），含 Wilson 95% CI。

## 验收目标
- 上述 4 个集合 top5 召回提升；`negative:clean` / `negative:novel` 不下降；core_v2 整句行不退化。
- `--mode word` 结果逐行不变。
- 延迟：单键 p50 <= 3 ms、p99 <= 10 ms、max <= 16 ms（`--gate phase3`）。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 评测集与评测器冻结，不得为提分修改。
- 核心 crate 严禁引入平台专有依赖；禁止流水账注释，保留信道代价 / 延迟预算的 Why 注释。
- L3 评测需要 task-07 的 lm-neural 纠错风格代码：只在临时测量分支合入 `feat/task-07-neural-distillation`，任务分支本身不含其提交；不合入 main、不 push、不重装在线守护进程。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [DONE — 待用户确认合并；未合入 main、未 push、未重装在线守护进程]
- Changed Files: `crates/engine/src/profile.rs`（新增 7 个 profile 字段，基准值 = 旧行为；`SENTENCE_PROFILE` 改值）、`crates/engine/src/confusables.rs`（罕见收录词 / 末尾未闭合 token 的近邻展开）、`crates/engine/src/lib.rs`（末词近邻、空格编辑门控 / 两处编辑组合 / token 预算，单测）、本任务单。`WORD_PROFILE` 未改。
- 根因（逐行诊断未召回行：ood_dev 132 / academic 206 / tm2 110 / noise_v2 202 行，按"正确词不在槽内 / 在槽内但未解码出 / token 数不同"三类）：
  1. 多词预编辑的**末尾未闭合 token 从不展开闭合近邻**：整句按提交时末词已完整，末词漏首字母（`train sers -> users`、`many ears -> years`）永远召不回。
  2. **罕见收录词**（`ord`、`ven`，log_freq < -6.0）被 `realword_neighbor_min_raw_log_freq` 提前返回，`ord -> word`、`ven -> even` 无候选。
  3. **空格编辑门控**要求至少一侧非词，而词典含大量罕见缩写碎片（`gr` -5.5、`ing` -5.1、`ly`、`nd`、`ware`、`fore`），`gr eat`、`us ing`、`ab out`、`soft ware` 一律不生成备选。
  4. 每个备选只含一处空格编辑，`th eatari ja guar` 类多处错误不可达。
  5. 未修（排序层面，留给 L3 / 后续）：词格 n-best 被末词屈折变体占满（person/parson/pierson）；缺撇号所有格 `teachers -> teacher's` 在槽内但分低（academic `contraction` 50 行主要是此类）。
- 修复（全部经 `SENTENCE_PROFILE` 开启，基准值保持旧行为）：`confusables.rare_realword_log_freq = -6.0` / `rare_realword_base_penalty = 1.5`（罕见收录词按疑似错拼展开近邻，不叠加频差惩罚）；`confusables.open_token_neighbors = true` / `open_token_neighbor_penalty = 0.5`（末尾 token 只对非词与罕见词展开，常见真词末词不展开以免复数被改单数）；`space_edits.rare_side_log_freq = -4.5`（合并 / 错位门控把罕见碎片视为非词；收录词的拆分仍按原规则只拆极罕见粘连形）；`space_edits.max_edits_per_alternative = 2`、`max_alternatives = 12`、`max_alternative_tokens = 56`（每键备选总 token 预算，长句超预算后不再追加，守住 max <= 16 ms）。调参过程：rare 阈值 -5.5 时 `negative:clean` top1 掉 0.5–1.7pt（keyboards/outputs/salads 被改），-6.0 后回到 CI 内（每集至多 1 行）；末词真词近邻导致 prices->price 类误改，已关闭。
- **+ L3 v2**（`anime-l3-360m-v2.Q4_K_M.gguf`，临时测量分支 `scratch/t19-l3-base` = main + task-07、`scratch/t19-l3-new` = 本分支 + task-07，`--features vulkan`，Wilson 95% CI）：

| 集合 | n | top1 前 | top1 后 | top5 前 | top5 后 |
|---|---|---|---|---|---|
| ood_dev_v1 TOTAL | 1030 | 83.8 (81.4–85.9) | 87.5 (85.3–89.4) | 86.7 (84.5–88.6) | 90.6 (88.6–92.2) |
| ood_dev_v1 negative:clean | 300 | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) |
| ood_dev_v1 negative:novel | 150 | 100.0 (97.5–100.0) | 100.0 (97.5–100.0) | 100.0 (97.5–100.0) | 100.0 (97.5–100.0) |
| ood_academic_v1 TOTAL | 1030 | 75.8 (73.1–78.3) | 77.9 (75.2–80.3) | 80.1 (77.5–82.4) | 82.1 (79.7–84.4) |
| ood_academic_v1 negative:clean | 300 | 99.7 (98.1–99.9) | 99.7 (98.1–99.9) | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) |
| ood_academic_v1 negative:novel | 150 | 99.3 (96.3–99.9) | 99.3 (96.3–99.9) | 100.0 (97.5–100.0) | 100.0 (97.5–100.0) |
| ood_tm2_v1 TOTAL | 600 | 74.3 (70.7–77.7) | 78.2 (74.7–81.3) | 81.7 (78.4–84.6) | 86.2 (83.2–88.7) |
| ood_tm2_v1 negative:clean | 200 | 99.0 (96.4–99.7) | 99.0 (96.4–99.7) | 100.0 (98.1–100.0) | 100.0 (98.1–100.0) |
| noise_v2 TOTAL | 1650 | 84.2 (82.3–85.9) | 90.0 (88.5–91.4) | 87.3 (85.6–88.9) | 93.2 (91.9–94.3) |
| noise_v2 negative:clean | 300 | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) |
| core_v2 TOTAL | 1375 | 81.5 (79.4–83.5) | 81.5 (79.4–83.5) | 96.3 (95.2–97.2) | 96.3 (95.2–97.2) |
| core_v2 sentence | 200 | 94.0 (89.8–96.5) | 94.0 (89.8–96.5) | 96.5 (93.0–98.3) | 96.5 (93.0–98.3) |

  逐行对照：noise_v2 / ood_dev / academic / tm2 / core_v2 Top-1 **丢失 0 行**，新增 96 / 38 / 21 / 23 / 0 行。ood_dev 按标签 top5（前 -> 后）：noise:multi 45.0 -> 61.0、space_extra 70.0 -> 95.0、boundary_gemination 70.0 -> 76.7、leading_drop 76.7 -> 81.7、space_shift 91.7 -> 95.0，其余不变。`negative:clean` / `negative:novel` 各集逐值不变。
- **L1+L2 单独**（同 L2 / 词库，Wilson 95% CI）：

| 集合 | n | top1 前 | top1 后 | top5 前 | top5 后 |
|---|---|---|---|---|---|
| ood_dev_v1 TOTAL | 1030 | 66.9 (64.0–69.7) | 69.7 (66.8–72.4) | 86.3 (84.1–88.3) | 90.2 (88.2–91.9) |
| ood_dev_v1 negative:clean | 300 | 88.7 (84.6–91.8) | 88.3 (84.2–91.5) | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) |
| ood_dev_v1 negative:novel | 150 | 50.0 (42.1–57.9) | 49.3 (41.4–57.3) | 100.0 (97.5–100.0) | 100.0 (97.5–100.0) |
| ood_academic_v1 TOTAL | 1030 | 59.3 (56.3–62.3) | 60.6 (57.6–63.5) | 78.9 (76.3–81.3) | 80.9 (78.4–83.2) |
| ood_academic_v1 negative:clean | 300 | 88.0 (83.8–91.2) | 88.0 (83.8–91.2) | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) |
| ood_academic_v1 negative:novel | 150 | 70.7 (62.9–77.4) | 70.7 (62.9–77.4) | 100.0 (97.5–100.0) | 100.0 (97.5–100.0) |
| ood_tm2_v1 TOTAL | 600 | 67.2 (63.3–70.8) | 70.3 (66.6–73.8) | 80.7 (77.3–83.6) | 84.8 (81.7–87.5) |
| ood_tm2_v1 negative:clean | 200 | 94.5 (90.4–96.9) | 94.5 (90.4–96.9) | 100.0 (98.1–100.0) | 100.0 (98.1–100.0) |
| noise_v2 TOTAL | 1650 | 78.7 (76.6–80.6) | 84.0 (82.2–85.7) | 87.1 (85.4–88.6) | 92.9 (91.6–94.1) |
| noise_v2 negative:clean | 300 | 98.0 (95.7–99.1) | 97.7 (95.3–98.9) | 100.0 (98.7–100.0) | 100.0 (98.7–100.0) |
| core_v2 TOTAL | 1375 | 83.1 (81.1–85.0) | 83.1 (81.1–85.0) | 96.3 (95.2–97.2) | 96.3 (95.2–97.2) |
| core_v2 sentence | 200 | 89.0 (83.9–92.6) | 89.0 (83.9–92.6) | 96.5 (93.0–98.3) | 96.5 (93.0–98.3) |

  逐行：Top-1 丢失 2 / 3 / 1 / 1 / 0 行，新增 90 / 32 / 14 / 20 / 0 行（noise_v2 / ood_dev / academic / tm2 / core_v2）。`negative:clean` 至多 1 行波动（noise_v2 `good bye -> goodbye`、ood_dev `router -> outer`），均在 CI 内；core_v2 逐值不变。
- **单词模式不变**：`--mode word --dump-rows` 5 个冻结集与改动前 `cmp` 逐字节一致（core_v2 80.65/92.51、noise_v2 45.88/47.03、ood_dev 51.07/52.62、academic 46.21/48.35、tm2 50.17/53.50）。
- **延迟**（L1+L2 整句模式，每集交替跑 3 次，单键 us，前 -> 后，取 3 次最大值）：core_v2 p50 100 -> 106、p99 1260 -> 1430、max 3677 -> 4471；noise_v2 237 -> 266 / 2209 -> 2990 / 8985 -> 10062；ood_dev 321 -> 363 / 3151 -> 4661 / 13158 -> 14631；academic 263 -> 325 / 2357 -> 3588 / 7131 -> 10851；tm2 245 -> 266 / 2533 -> 3027 / 7967 -> 9852。全部满足 p50 <= 3 ms、p99 <= 10 ms、max <= 16 ms。`--gate phase3`：core_v2 通过；其余 4 集改动前后同样因 exact / typo_simple 等类别无样本、context_realword.top3 判定失败（与本改动无关，延迟三项均通过）。未加预算时 ood_dev 长句 max 22.8 ms，已由 `max_alternative_tokens` 解决。
- Verification Output：`make check` Finished；`make test` 79 passed / 0 failed（含新增 `sentence_profile_recalls_rare_fragments_open_last_word_and_two_space_edits`，`hen said` 用例改为整句模式召回 he、基准 profile 保留 hen）；`make eval`（smoke）TOTAL 28.6%，与 main 相同；`make verify-infra` INFRA OK；`./scripts/verify-boundaries.sh .agents/tasks/task-19-sentence-recall-v2.md --base main` BOUNDARY OK，零越界。
- 注意：共享 `CARGO_TARGET_DIR` 与并行会话同时构建时，cargo 会复用别的 worktree 的 `anime-lexicon` rlib（源文件 mtime 更旧），表现为莫名的字段缺失编译错误或"改动不生效"；构建前 `touch` 本 worktree 的 `crates/**/*.rs` 并立即拷贝产物可规避。
