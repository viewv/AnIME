---
task_id: "task-12-ood-dev-eval"
worktree: "../wt-anime-ood-dev"
allowed_paths:
  - "eval/**"
  - "scripts/data_pipeline/**"
  - "data/MANIFEST.yaml"
  - ".agents/contracts/eval-schema.md"
  - ".agents/tasks/task-12-ood-dev-eval.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/ipc-protocol.md"
  - ".agents/contracts/data-format.md"
  - "crates/**"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/eval-schema.md"
  - ".agents/contracts/data-format.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-12-ood-dev-eval.md"
---

## Objective (目标)
新增贴近开发者日常键入的跨领域评测集 `ood_dev_v1`：取自 OpenAssistant oasst1 中**按对话树留出**的英文用户提问（真人撰写的现代 / 技术 / 日常文本），包含干净反例、单错误、多错误，以及含词典外新词（技术词、新名词）的干净句 `negative:novel`，用于衡量"新词新句式"体验与"不误改新词"。`ood_v1`（LibriSpeech 文学）降级为仅参考。

## Background
- 用户以开发者身份使用 AnIME，关注新单词与新句式；`ood_v1` 的 19 世纪文学失败（`nay stay`、`Jill`、`mister`）不代表目标用户（见 task-11 讨论）。
- 现有 L2 已用 oasst1 全量 prompter / assistant 训练；留出部分必须从训练语料中剔除并重建 L2，否则评测有泄漏。

## 核心子任务
1. **留出划分**：按 `message_tree_id` 的 CRC32 确定性抽取约 15% 对话树作为评测专用；`build_oasst_text.py` 输出训练文本时跳过这些树（同一对话不同时出现在训练与评测）。
2. **生成 `eval/benchmarks/ood_dev_v1.tsv`**（`build_eval_v3.py`）：留出树的英文 prompter 消息经 Markdown 清洗后取 4–14 词子句；单错误 8 类各 >= 60、`noise:multi` >= 100、`negative:clean` >= 300；含词典外 token（字母词、非纯数字）的干净子句标 `negative:novel` >= 100。噪声只作用于词典词。
3. **契约**：`eval-schema.md` §1 保留标签新增 `negative:novel`（输入含词典外新词的干净句，`input == expected`）。
4. **复核**：沿用 task-10 分歧裁决（仅复核被引擎改动的干净 / 新词句），裁决追加到 `eval/review/negative_review.tsv`，迭代至零未复核分歧。
5. **重建 L2**：以现行配置（MultiWOZ x20 + oasst1 assistant x30 / prompter x100，留出树已剔除）重建并评测；是否替换已安装模型由用户决定。
6. **基线**：L1+L2 与 L1+L2+L3（SmolLM2-360M）在 `ood_dev_v1` 上的指标写入 Evidence；`ood_v1` 在 ROADMAP / 评测说明中标注为仅参考。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 契约改动仅限新增 `negative:novel` 标签定义，不改既有格式与指标语义。
- 不下载新数据（oasst1 已登记）；评测源与训练语料必须按对话树隔离，并经 `eval/**` 去泄漏。
- 冻结既有评测集（core_v1 / core_v2 / noise_v1 / noise_v2 / ood_v1 / ood_tm2_v1）。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
