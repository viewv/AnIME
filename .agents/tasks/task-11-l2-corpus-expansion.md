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
- Status: [CLOSED — 负面结果为主] 用户仅批准 oasst1；L2 语料扩展对日常口语无显著收益（未达 `ood_tm2_v1 >= 72%` 目标）。经用户决定：采用 oasst1 加权版（b）以补充现代词汇并替换已安装模型，后续领域外与语义问题交由 L3（Task 07）。
- Changed Files:
  - `scripts/data_pipeline/build_oasst_text.py`（新增）：oasst1 英文、审核通过、非合成消息 → prompter / assistant 纯文本，去除代码块 / 内联代码 / URL / Markdown。
  - `scripts/data_pipeline/build_ngram_kn.py`：`--weighted-text PATH:REPEAT` 任意来源加权；`scripts/data_pipeline/test_pipeline.py`（26 tests）、`README.md`（正式构建命令加入 oasst1 权重）。
  - `data/MANIFEST.yaml`：`oasst1`（Apache-2.0，HF revision `fdf72ae0`，SHA256，署名），共 34 个数据源。
- Verification Output（L1+L2，task-09 引擎，`LM_LAMBDA = 0.85`）：
  - oasst1 转换：15,452 条 prompter（335,340 词）+ 24,137 条 assistant（3,023,540 词）。
  - **权重网格（200 MB 预算内）**，top1：

    | 模型 | 规模 | core_v2 | noise_v2 | ood_v1 | ood_tm2_v1 | tm2 negative:clean |
    |---|---|---|---|---|---|---|
    | 基线 `ngram_kn_r20_v3` | 172 MB，k_tri=11 | 83.1 | 78.7 | 74.7 | 66.3 | 95.0 |
    | a（assistant x10 / prompter x30） | 173 MB，k_tri=12 | 83.3 | 78.7 | 73.5 | 66.8 | 95.5 |
    | **b（x30 / x100，采用）** | 176 MB，k_tri=14 | 83.1 | 78.8 | 73.7 | **67.2** | 94.5 |
    | c（x60 / x200） | 176 MB，k_tri=14 | 83.2 | 78.5 | 74.2 | 66.8 | 93.5 |

  - **逐样本（ood_tm2_v1，基线 → b）**：16 胜 11 负。胜例集中在现代词汇（`betflix -> Netflix`、`john widk -> John Wick`、`led zeppelin`（原 `lead`）、`by Katy Perry`、`cheese burgers`（原 `burghers`））；负例多为固定体积预算下 oasst 三元组挤出旧搭配后的回退（`movie -> move`、`brighton -> brighten`）——剪枝阈值随加权从 11 升到 14。
  - **放宽剪枝（容量是否瓶颈）**：`big_base`（20M 三元组预算，实保留 19.8M，426 MB）core_v2 83.0、noise_v2 78.6、**ood_v1 76.7（+2.0）**、ood_tm2_v1 67.2（+0.9，CI 63–71）；`big_b`（+oasst1，379 MB）83.0 / 78.6 / 76.0 / 67.0；`LM_LAMBDA` 0.7 / 1.0 均不优于 0.85。结论：容量只提升训练语料覆盖的领域（文学），日常口语的瓶颈是缺少口语语料与 3-gram 的短上下文，不是体积；2.5 倍内存换 < 1pp 不划算。
  - 采用模型 b：`ngram_oasst_b`，175,722,357 bytes，SHA256 前缀 `7b977dad`；9.48 亿词、去泄漏丢弃 3,529,704 句、词表 267,832、3,939,531 二元组（cc >= 7）+ 6,677,959 三元组（count >= 14）。`ood_v1` -1.0pp 处于验收允许边界，用户已接受。
  - 单键延迟在桌面负载下波动大（基线同条件复测 max 16.8–20.5 ms），p99 稳定在 1.7–4.9 ms；单次 max 门禁对系统噪声过敏，建议后续改为多次运行取中位数。
