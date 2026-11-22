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
- Status: [DONE — 待评审] Phase A 全部完成；Phase B 经用户批准仅接入 Taskmaster-2（Tatoeba CC0 子集与 Rust Book 未获批，技术文本领域仍缺）。
- Changed Files:
  - `.agents/contracts/eval-schema.md`：§3 新增交互指标定义；§4 本机打字记录隐私约定（未改既有 TSV 格式与指标语义）。
  - `crates/eval/src/{metrics,cli,main,report}.rs`：`--interactive`（省键率 Top-1/Top-3、Top-1 挤出次数、进入后丢失率、平均选择序号）与 Wilson 95% 置信区间；排名检查不计入单键延迟；新增单测。
  - `scripts/data_pipeline/build_eval_v3.py`（新增）、`replay_local_log.py`（新增）、`build_eval_v2.py`（抽出 `clauses_from_texts`，noise_v1 / core_v2 输出逐字节不变）、`test_pipeline.py`（23 tests）、`README.md`。
  - `eval/benchmarks/noise_v2.tsv`（1650）、`ood_v1.tsv`（600，LibriSpeech CRC32 留出）、`ood_tm2_v1.tsv`（600，Taskmaster-2）；`eval/review/negative_review.tsv`（163 条裁决，39 条源句错误剔除）。
  - `data/MANIFEST.yaml`：Taskmaster-2 四个领域（CC-BY-4.0，commit `d92cb6af`，SHA256 与署名）；LibriSpeech 留出用途说明（共 33 个数据源，`--verify-only` 通过）。
- Verification Output：
  - **反例复核方法（分歧裁决）**：导出 MultiWOZ / LibriSpeech / Taskmaster-2 干净候选 → 以 task-09 引擎评测 → 只人工裁决被改动的句子（源句错误 exclude，引擎误判 keep）→ 取样改为 (seed, 类型, 句子) CRC32 稳定排序后迭代至零未复核分歧。未被改动的候选默认保留（可能仍含未被发现的源句错误，但不影响"误改率"的有效性）。
  - **L2 重建**：新评测集写入 `eval/` 后重跑 `build_ngram_kn.py`（MultiWOZ x20），去泄漏丢弃 1,811,164 句；产物 `ngram_kn_r20_v3`（172,038,722 bytes，3,591,952 二元组 + 6,794,681 三元组）。重建前后 top1：core_v2 83.0 → 83.1、noise_v1 81.6 → 81.6、noise_v2 79.0 → 78.7、**ood_v1 75.8 → 74.7**（旧 L2 训练过这些 LibriSpeech 句子，重建去除泄漏后的真实值）、ood_tm2_v1 66.7 → 66.3；除 ood_v1 外均在置信区间内波动。
  - **基线（task-09 引擎 + `ngram_kn_r20_v3`，L1+L2）**：

    | 集合 | n | TOTAL top1（95% CI） | sentence top1 | negative:clean | noise:multi | 省键率 Top-1 / Top-3 | 平均选择序号 |
    |---|---|---|---|---|---|---|---|
    | core_v2 | 1375 | 83.1%（81.1–85.0） | 90.0% | — | — | 15.0 / 28.3% | 1.51 |
    | noise_v2（MultiWOZ） | 1650 | 78.7%（76.7–80.6） | 79.3% | 97.3% | 50.7% | 7.3 / 9.2% | 2.27 |
    | ood_v1（文学） | 600 | 74.7%（71.0–78.0） | 74.8% | 96.5% | 35.0% | 3.7 / 5.7% | 2.51 |
    | ood_tm2_v1（日常口语） | 600 | **66.3%**（62.5–70.0） | 68.2% | 95.0% | **21.2%** | 2.3 / 4.8% | 2.88 |

    ood_tm2_v1 单错误类：adjacent_key 82.5、contraction 85.0、space_shift 67.5、space_missing 60.0、leading_drop 55.0、space_extra 45.0、boundary_gemination 42.5、realword_edit 40.0（n=40/类，CI 约 ±15pp）。
  - **结论**：领域外（日常口语）比 MultiWOZ 低约 12pp，多错误句 21–51%；复核后的反例误判以专名（`Steven -> Seven`、`la galaxy -> law galaxy`）、常见口语词（`who -> why`、`coke -> come`、`movies -> moves`）与时态/单复数（`has -> had`、`prices -> price`）为主，是 task-11（领域语料）与后续召回 / 打分工作的直接靶点。
  - 单键延迟（新 L2）：p99 1.6–3.5 ms、max 5.5–12.3 ms，五个集合 `total.key_p99_us<=10000`、`total.key_max_us<=16000` 门禁全部 PASS。
  - `make check`（0 warning）/ `make test`（65 passed, 0 failed）/ `make eval` / `make verify-infra`（INFRA OK）/ `python3 scripts/data_pipeline/test_pipeline.py`（23 OK）/ `./scripts/verify-boundaries.sh .agents/tasks/task-10-eval-v3.md`（含 `--base feat/task-09-sentence-recall --tracked-only`，15 个文件）BOUNDARY OK。
