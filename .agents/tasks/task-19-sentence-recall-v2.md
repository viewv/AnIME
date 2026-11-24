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
- Status: [PENDING]
- Changed Files:
- Verification Output:
