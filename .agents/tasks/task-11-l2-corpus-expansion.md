---
task_id: "task-11-l2-corpus-expansion"
worktree: "../wt-anime-corpus"
allowed_paths:
  - "scripts/data_pipeline/**"
  - "data/MANIFEST.yaml"
  - "crates/lm-ngram/src/bin/**"
  - "crates/lm-ngram/src/viterbi.rs"
  - ".agents/tasks/task-11-l2-corpus-expansion.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "eval/**"
  - "crates/eval/**"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/engine/**"
  - "crates/lm-neural/**"
  - "crates/animed/**"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/data-format.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-11-l2-corpus-expansion.md"
---

## Objective (目标)
为 L2 补充许可合规的**日常书面 / 口语**与**技术文本**语料，缩小领域外差距：task-10 基线中日常口语 `ood_tm2_v1` TOTAL top1 66.3%，比 MultiWOZ 的 `noise_v2`（78.7%）低约 12pp。在不改变 `ANIMENGR` v1 格式与引擎代码的前提下，通过语料、按来源加权与 Viterbi 权重重调提升领域外表现，并保持领域内与干净输入不退化。

## Background
- 现有 L2（`ngram_kn_r20_v3`）= LibriSpeech 全量（文学）+ MultiWOZ 2.2 train/dev x20（订酒店对话），共约 8.3 亿词；复核反例中的误改集中在常见口语词（`who->why`、`movies->moves`、`coke->come`）与时态 / 单复数，说明 L2 缺少日常口语统计。
- `data-format.md` 白名单：MIT / Apache-2.0 / BSD / MPL-2.0 / ISC / CC0 / Public Domain / CC-BY-4.0（CC-BY-SA 仅限词表，不得用于 L2）。
- 已调研候选（task-11 启动时，须在执行时复核许可原文）：
  - **OpenAssistant oasst1**（Apache-2.0）：志愿者手写的问答消息，取英文；最接近真实键入文本。
  - **Cosmopedia** `stories` / `wikihow` 子集（Apache-2.0，Mixtral 合成）：日常叙事与操作说明；合成文风冗长，只取子集并低权重。
  - **Kubernetes 官方文档** `kubernetes/website` `content/en`（CC-BY-4.0，GitHub API 确认）：技术文本。
  - 可选：Tatoeba CC0 子集（日常短句）。
  - **不用于训练**：Taskmaster-1/3（与 `ood_tm2_v1` 的 food / movies 领域重叠，会让该留出集失去领域外意义）；许可不明的 Ubuntu Dialogue、GitHub Typo Corpus。

## 核心子任务
1. **获批后下载与登记**：仅下载用户批准的来源，固定版本 / commit，`data/MANIFEST.yaml` 登记 URL、版本、协议、SHA256、署名与"仅训练 L2"用途。
2. **语料转换**（`scripts/data_pipeline/`）：各来源转为每行一段的纯文本（Markdown 去代码块 / 表格 / 链接语法，对话取消息正文，过滤非英文），记录行数 / 词数统计。
3. **按来源加权构建**：`build_ngram_kn.py` 支持任意来源的重复权重（沿用 `--dialogue-repeat` 思路，泛化为 `--text-repeat` 或来源清单），以 `eval/**` 去泄漏；对 2–3 组权重网格构建并比较。
4. **Viterbi 权重重调**：新 L2 下重扫 `LM_LAMBDA`（`crates/lm-ngram/src/viterbi.rs`），只在显著改善时修改并写清 Why 注释。
5. **资源预算**：`ngram.animengr.bin` <= 200 MB（必要时调整剪枝预算），`--gate phase3` 与 `total.key_p99_us<=10000`、`total.key_max_us<=16000` 通过。

## 验收目标（L1+L2，task-09 引擎）
- `ood_tm2_v1` TOTAL top1 >= 72%（基线 66.3%），sentence top1 显著提升。
- `ood_v1`（文学）、`core_v2`、`noise_v2` TOTAL top1 下降不超过 1pp；`core_v2` 各类别不退化超过 1pp。
- 五个评测集 `negative:clean` top1 均不低于基线（core_v2 无此标签，看 `exact`）。
- 报告各集合 top1 的 Wilson 95% 置信区间，判断差异是否显著。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- **下载任何数据前必须获得用户批准**；许可证以下载时的原文为准，不确定即不用。
- 评测集与评测器冻结；新训练语料必须经 `eval/**` 整句 + 5-gram 去泄漏。
- 合成语料（Cosmopedia）只作补充，不得成为最大权重来源；在 Evidence 中报告其单独贡献。
- 保留平滑、加权与剪枝相关的 Why 注释，禁止流水账注释。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
