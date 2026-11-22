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
- Status: [DONE — 待评审] `ood_dev_v1` 已生成并复核；是否把去泄漏重建的 L2 替换为已安装模型由用户决定。
- Changed Files:
  - `.agents/contracts/eval-schema.md`：§1 新增 `negative:novel` 标签定义。
  - `scripts/data_pipeline/build_oasst_text.py`：按 `message_tree_id` CRC32 留出约 15% 对话树（`is_heldout_tree`，空 ID 不算留出），训练文本跳过留出树；`heldout_prompter_paragraphs` 供评测复用。
  - `scripts/data_pipeline/build_eval_v3.py`：`dev_clauses`（4–14 词，代词 `I` 视为已知词，Zipf < 2.5 或词典外的字母词视为新词，<= 2 个新词）、`build_novel_negatives`；新词句只用于 `negative:novel`，不加噪。
  - `scripts/data_pipeline/test_pipeline.py`（29 tests）、`eval/benchmarks/ood_dev_v1.tsv`（新增，1030 行）、`eval/review/negative_review.tsv`（追加 128 条裁决，20 条剔除）。
- Verification Output：
  - 留出：5,717 条消息（对话树级）不进入训练；留出树英文提问 3,420 段 → 2,766 条常见词子句 + 366 条新词子句。
  - `ood_dev_v1`：8 类单错误各 60、`noise:multi` 100、`negative:clean` 300、`negative:novel` 150。复核 2 轮收敛（剔除源句错拼 `equiped` / `substract` / `fonction` / `recomend` / `synatx` / `explaination` / `publically` / `chat gtp` / `Myer briggs`、非英文与残片）；既有冻结评测集逐字节不变（曾误剔除 1 条 LibriSpeech 源句 `how eloquent of wo is that sound` 导致 ood_v1 变动，已改回 keep 并复核恢复）。
  - 去泄漏 L2：`ngram_dev_b`（MultiWOZ x20 + oasst1 assistant x30 / prompter x100，留出树已剔除），178,626,666 bytes，9.29 亿词，去泄漏丢弃 3,407,419 句；`ood_dev_v1` 1,090 条参考文本全部在去泄漏参照中。
  - **基线（task-09 引擎，L1+L2）**：

    | 指标（ood_dev_v1 top1） | 已安装 L2（训练含留出树，泄漏） | **去泄漏 L2** | 去泄漏 L2 + SmolLM2-360M |
    |---|---|---|---|
    | TOTAL（95% CI） | 78.9% | **62.9%（59.9–65.8）** | 66.4%（63.5–69.2） |
    | sentence / context_realword | 78.6 / 85.0 | 63.0 / 61.7 | 65.9 / 75.0 |
    | negative:clean | 100.0 | 88.7 | 88.7 |
    | **negative:novel** | 31.3 | **22.7** | 24.7 |
    | noise:multi | 45.0 | 32.0 | 36.0 |
    | leading_drop / boundary_gemination / space_shift | 83.3 / 88.3 / 95.0 | 53.3 / 56.7 / 58.3 | 58.3 / 60.0 / 73.3 |

  - **结论**：开发者文本是所有评测集中最弱的领域（L1+L2 62.9%，低于日常口语 67.2%、MultiWOZ 78.8%）；含新词的干净句约 77% 被误改（`ChatGPT -> Chapter`、`numpy -> jump`、`PyTorch -> Porch`、`Nginx -> Engine`、`JSON -> JASON`、`mkdir -> direct`、`gitlab -> italy`、`fastapi -> fatality`），L3 几乎不改善（+2pp）。"不误改新词"是比领域语料更优先的问题，指向可挂载技术词典与 L1 / L3 的新词保护。泄漏对比（78.9% vs 62.9%）也说明按对话树留出与去泄漏是必要的。
  - **已知偏差**：`build_eval_v2.extract_clauses` / `clauses_from_texts` 的源句过滤要求每个 token 都在词典 TSV 中，而 TSV 不收录代词 `I`，导致 noise_v1 / noise_v2 / ood_v1 / ood_tm2_v1 系统性缺少含独立 `I` 的句子；这些集合已冻结未改，`ood_dev_v1` 已修正。
  - `ood_v1`（LibriSpeech 文学）按用户意见降级为仅参考（ROADMAP 已注明）。
