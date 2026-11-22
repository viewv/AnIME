---
task_id: "task-10-eval-v3"
worktree: "../wt-anime-eval-v3"
allowed_paths:
  - "crates/eval/**"
  - "eval/**"
  - "scripts/data_pipeline/**"
  - "data/MANIFEST.yaml"
  - ".agents/contracts/eval-schema.md"
  - ".agents/tasks/task-10-eval-v3.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/ipc-protocol.md"
  - ".agents/contracts/data-format.md"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
  - "crates/lm-neural/**"
  - "crates/engine/**"
  - "crates/animed/**"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/eval-schema.md"
  - ".agents/contracts/data-format.md"
  - ".agents/contracts/core-api.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-10-eval-v3.md"
---

## Objective (目标)
让评测从"订酒店场景下打完整句后的最终候选"扩展为更接近真实打字体验的标尺：跨领域留出集、打字过程交互指标、更大样本量、人工复核的反例集，以及在本机回放用户自己打字记录的工具。本任务只改评测与数据管线，不改引擎。

> 叠加在 `feat/task-09-sentence-recall` 之上（task-08 → task-09 → task-10）。

## Background (现状缺口，task-09 后实测)
- `sentence` / `context_realword` 几乎全部源自 MultiWOZ（剑桥订酒店/餐厅），非该领域样本仅 114 条；L2 已加入 MultiWOZ train，领域内指标高估通用体验。
- `noise_v1` 每句只注入一种合成错误；每类 60 条、核心集每类 200 条，95% 置信区间约 ±6pp / ±3pp。
- 只评测最终状态：不衡量按键过程中的候选稳定性、选中位置与省键率。
- `negative:clean` 含源句自身错误（`tell me he postcode`、`thanks you a lot` 等），引擎改对被记为误改（task-09 Blocked 第 2 条）。
- `data/raw/github-typo-corpus/` 为空目录且未登记 MANIFEST；当前本机可用的非 MultiWOZ 句级语料只有 LibriSpeech（已全量进入 L2 训练）。

## 核心子任务
### Phase A（无需下载）
1. **交互指标（需扩展 `eval-schema.md` §3，仅新增指标，不改现有字段语义）**：`anime-eval --interactive` 逐键回放并统计：
   - `first_hit_keys`：期望答案首次进入 Top-1 / Top-3 时已敲键数，及相对全长的省键率（单词类别）；
   - `flip_rate`：最后 N 键内 Top-1 发生变化的次数 / 样本（候选跳动）；
   - `mean_select_index`：最终期望答案的平均选择序号（未命中计为 K+1）。
2. **复核反例集**：对 `negative:clean` 逐条复核，剔除源句本身有拼写/语法错误的句子并在 notes 记录剔除原因统计；扩充到 >= 400 条已复核干净句（多来源）。
3. **更大且多错误的噪声集 `noise_v2`**：沿用 `build_eval_v2.py` 算子，每类 >= 150 条，并新增 `noise:multi`（同句 2–3 处不同类型错误）>= 150 条；`noise_v1` 冻结保留。
4. **跨领域留出集（本地可得部分）**：从 LibriSpeech 以确定性哈希切出 >= 300 句留出句（文学领域，作为非 MultiWOZ 对照）生成 `ood_v1.tsv`（干净反例 + 单错误 + 多错误）；写入 `eval/` 后用 task-08 的 `build_ngram_kn.py` 重建 L2，使去泄漏过滤生效，并在 Evidence 记录重建前后指标。
5. **本机打字记录回放工具**：`scripts/data_pipeline/replay_local_log.py` 读取用户本机提供的按键/上屏记录（TSV，格式写入 README），转换为评测 TSV 并调用 `anime-eval`；**严禁**入库或进入任何训练语料（输出路径默认 `data/compiled/local/`，已被 `.gitignore`）。本任务不实现采集端。
### Phase B（需用户批准下载后执行）
6. 调研许可证在 `data-format.md` 白名单内的**日常对话 / 邮件与工作文书 / 技术文本**英文句级语料，列出来源、协议、体积与取舍，**提交用户批准后**再下载、登记 MANIFEST，生成对应 `ood_*` 留出集。

## 验收
- `anime-eval` 新增交互指标有单测；对 task-09 引擎输出 `core_v2` / `noise_v2` / `ood_v1` 的交互指标与准确率基线写入 Evidence。
- 所有新评测集可由脚本确定性重建（固定 seed），并在 MANIFEST notes 写明来源与用途（仅评测）。
- L2 重建后 `core_v2` / `noise_v1` 指标与 task-09 Evidence 差异在 Evidence 中说明。
- 报告每个关键指标的样本数与 95% 置信区间（Wilson）。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 契约改动范围仅限 `eval-schema.md` 新增交互指标与本地回放数据约定，不得改变现有 TSV 格式与既有指标语义。
- 新评测集源句必须经 `build_ngram_kn.py` 去泄漏与 L2 训练语料隔离；`core_v1` / `core_v2` / `noise_v1` 冻结。
- 用户本机打字记录只在本机处理，不进 Git、不进训练、不出现在任何报告的原文中（报告只给聚合指标）。
- 下载任何新数据前必须先获得用户批准（Phase B）。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
