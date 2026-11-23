---
task_id: "task-07-neural-distillation"
worktree: "../wt-anime-l3-correction"
allowed_paths:
  - "scripts/data_pipeline/**"
  - "data/MANIFEST.yaml"
  - "crates/lm-neural/**"
  - "crates/eval/**"
  - "crates/animed/**"
  - "ROADMAP.md"
  - ".agents/tasks/task-07-neural-distillation.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "frontends/**"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
  - "crates/engine/**"
  - "eval/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/data-format.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "make check && make test && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-07-neural-distillation.md"
---

## Objective (目标)
训练输入法专用的 **~360M 级条件纠错 L3 模型**：给定上文与用户**实际键入的原文**，判断 / 生成用户本来想输入的文本，替代当前只看通顺度的通用 SmolLM2 重排。模型保持 SmolLM2-360M 架构与 BPE 词表（llama.cpp GGUF 直接兼容），在用户的 Google Colab 上全参数监督微调，导出 GGUF 后接入 `crates/lm-neural`。

> 2026-11-23 重写：原"1.7B 教师软标签蒸馏"方案作废——实测 1.7B 通用模型在日常口语上也只补回 L1+L2 与 Top-5 上限间 28% 的差距，且通用 LM 不看用户原文，会把正确输入（新词、专名）改成高频词。合成噪声数据天然带精确标签，直接监督微调优于模仿教师。

## Background（task-10 / task-13 实测，L1+L2+L3 = 通用 SmolLM2-360M）
| 评测集 | L1+L2 | +通用 360M | L1+L2 Top-5 上限 |
|---|---|---|---|
| ood_dev_v1（开发者文本，主目标） | 62.9% | 66.4% | — |
| ood_tm2_v1（日常口语） | 67.2% | 69.8% | 80.3% |
| noise_v2（MultiWOZ） | 78.8% | 79.3% | 87.2% |
| ood_dev `negative:novel`（新词句原样保留） | 22.7% | 24.7% | — |
| ood_dev `noise:multi`（一句多错） | 32.0% | 36.0% | — |

通用模型的问题：只算 `P(候选 | 上文)`，不看用户原文 → 把 `nay` / `ChatGPT` / `Steven` 拉向高频词；只能重排 L1+L2 候选 → 多错误、空格错误召回不到时无能为力；135M 甚至降低干净句保留率。

## 方案：条件纠错模型
### 任务格式（纯文本标记，不扩充词表，GGUF / tokenizer 零改动）
```
<|ctx|> {上文末尾 ≤ 48 token} <|typed|> {用户键入的原文} <|fix|> {目标文本}<|end|>
```
- **打分模式（每次 L3 请求）**：`score(c) = Σ log P(c | ctx, typed)`，候选共享 `ctx + typed + <|fix|>` 前缀 KV（沿用现有前缀复用）。原文正确时"照抄"概率最高 → 天然保护干净输入与新词。
- **生成模式（停顿时，后续阶段）**：从同一前缀贪心 / 小束生成整句，作为额外候选，突破 L1+L2 召回上限（多错误、空格错误）。

### 训练数据（`scripts/data_pipeline/build_l3_sft.py`，可复现，固定 seed）
- **干净文本源**（均已登记 MANIFEST，许可在白名单内）：arXiv CS 摘要训练侧（task-14，元数据 CC0，按论文留出的评测论文不进入，约 25%）、oasst1 训练树（已排除 ood_dev 留出树，prompter 为主、assistant 为辅）、MultiWOZ 2.2 train / dev、LibriSpeech（小比例，只防遗忘）。Taskmaster 全部不用（ood_tm2 评测领域）；CC-BY-SA 来源（Wikipedia 错拼表等）不进入模型训练。
- **上文**：对话 / 会话中的前一轮或前一句（截断 ≤ 48 token），约 30% 样本留空上文。
- **噪声**：复用 `build_eval_v2` / `build_eval_v3` 的 8 类单错误算子 + `noise:multi` 多错误 + crate-ci typos（MIT）真实错拼对；按开发者 / 口语场景设定比例。
- **照抄样本 >= 35%**：`typed == 目标`，其中含词典外新词 / 专名 / 技术词的干净句单独加权（教模型不误改新词）。
- **去泄漏**：所有样本与 `eval/**` 做整句 + 5-gram 过滤（与 `build_ngram_kn.py` 同一参照集）；验证集从训练源按会话哈希切出。
- **规模**：约 1–2M 样本（4–8 千万 token），输出 `data/compiled/l3_sft/{train,val}.jsonl` 与统计；数据包 < 1 GB，上传 Colab / Google Drive。

### 训练（用户 Colab 执行，`scripts/data_pipeline/colab/train_l3_correction.ipynb` + 同名 `.py`）
- 基座：`HuggingFaceTB/SmolLM2-360M`（Apache-2.0）；**全参数**微调（T4 16 GB 可行：bf16/fp16 混合精度 + 8-bit AdamW + 梯度检查点）。
- 损失：仅对 `<|fix|>` 之后的目标 token 计算 NLL；阶段 2 可选加入 L1+L2 难负例的成对排序损失。
- 记录：训练 / 验证 loss、按噪声类型的验证 exact-match、照抄样本准确率。
- 导出：合并 → `convert_hf_to_gguf.py` → `llama-quantize` 生成 `Q4_K_M` 与 `Q8_0`，下载到 `data/compiled/models/anime-l3-360m-v1.*.gguf`；MANIFEST 登记基座许可、数据来源、超参数与 SHA256。

### 接入（`crates/lm-neural`、`crates/animed`、`crates/eval`）
- `NeuralModelConfig` 新增提示风格（`Generic` / `Correction`），由 GGUF 文件名或元数据识别；`Correction` 风格的重排使用上述模板与"整句候选"打分（不再只比较候选差异后缀）。
- `animed` 模型发现顺序：优先 `models/l3.gguf`（不变），可放置新模型或用 `ANIME_NEURAL_MODEL` 指定。
- `anime-eval`：L3 评测路径支持新提示风格；可选新增 `--dump-candidates` 导出 L1+L2 候选（供阶段 2 难负例）。
- 生成模式需要引擎接受 L3 生成的新候选，涉及 `crates/engine`（本任务禁止路径）——到阶段 4 时在 Blocked 申请。

## 阶段与检查点
| 阶段 | 内容 | 执行者 | 产物 / 验收 |
|---|---|---|---|
| P0 | 零训练基线：通用 360M / 1.7B 套用纠错模板打分 | Agent 本机 | 判断模板本身的收益，Evidence 记录 |
| P1 | 数据集构建 + 去泄漏 + 统计 | Agent 本机 | `train/val.jsonl`、数据卡 |
| P2 | Colab 全参数微调 + GGUF 导出 | **用户 Colab**（Agent 提供 notebook） | `anime-l3-360m-v1.{Q4_K_M,Q8_0}.gguf` + 训练日志 |
| P3 | `lm-neural` 纠错风格接入 + 全量评测 | Agent 本机 | 下表验收 |
| P4（可选） | 难负例排序微调；生成模式（需申请 engine 路径） | 用户 Colab + Agent | 多错误召回提升 |

## 验收目标（L1+L2+新 L3 vs L1+L2+通用 360M，`ngram` = 去泄漏 L2）
- `ood_dev_v1` TOTAL >= 72%（66.4%）、`negative:novel` >= 60%（24.7%）、`negative:clean` 不低于 L1+L2。
- `ood_academic_v1`（task-14，学术写作）TOTAL >= 66%（58.6%）、`negative:novel` >= 60%（34.0%）。
- `ood_tm2_v1` TOTAL >= 74%（69.8%）；`noise_v2` 与 `core_v2` 不退化；各集合 `negative:clean` 不低于 L1+L2。
- L3 延迟不劣于当前通用 360M（GPU 优化另行处理）；GGUF `Q4_K_M` <= 300 MB。
- 报告 Wilson 95% 置信区间；所有数字基于去泄漏 L2 与冻结评测集。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- **评测零泄漏**：训练数据经 `eval/**` 整句 + 5-gram 过滤；ood_dev 留出树、Taskmaster、MultiWOZ test 严禁进入训练。
- 只用 MANIFEST 已登记且许可在白名单内的数据；新增数据需用户批准下载。
- 模型权重、数据集不入 Git（`data/compiled/` 已忽略）；notebook 与脚本入库，可复现。
- 核心 crate 不引入平台专有依赖；保留提示模板、打分口径与延迟预算相关的 Why 注释。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [P3 完成 — 待评审] v2 模型 `anime-l3-360m-v2.Q4_K_M.gguf` 已评测并登记 MANIFEST；core_v2 `context_realword` 与 L3 推理延迟两项未达标（见下）。未合并 main。
- Changed Files（截至 P2 准备）：`crates/lm-neural/src/{config,lib,prompt,llama_backend}.rs`（`PromptStyle`、纠错模板打分、键入原文传入推理线程）、`crates/eval/src/{cli,main}.rs`（`--l3-prompt`）、`scripts/data_pipeline/build_l3_sft.py`、`scripts/data_pipeline/colab/train_l3_correction.{py,ipynb}`、`scripts/data_pipeline/test_pipeline.py`（34 tests）。
- Verification Output：
  - **P0 零训练基线**（去泄漏 L2，L1+L2+L3，top1）：

    | 模型 / 模板 | ood_dev TOTAL | clean | novel | multi | realword | ood_tm2 TOTAL | L3 p50 |
    |---|---|---|---|---|---|---|---|
    | 360M 通用模板 | 66.4 | 88.7 | 24.7 | 36.0 | 75.0 | 70.0 | 85 ms |
    | 360M 纠错模板 | 65.2 | 90.3 | 25.3 | 34.0 | 55.0 | 69.7 | 128 ms |
    | 1.7B 通用模板 | 66.8 | 90.0 | 24.0 | 36.0 | 71.7 | 70.8 | 145–339 ms |
    | 1.7B 纠错模板 | 66.0 | 91.3 | 25.3 | 35.0 | 55.0 | 69.3 | 455 ms |

    结论：未经训练的模型看不懂模板，看到键入原文后倾向"照抄"——干净句保留 +1.3–2pp，但真词纠错大幅下降（75 -> 55）、整体略降；模板本身不带来收益，必须微调。纠错模板打分整句候选（而非差异后缀），前缀更长，360M p50 85 -> 128 ms，P3 需优化（候选按 token 共享前缀已复用，可再做差异化打分或批量解码）。
  - **P1 数据集**：`train.jsonl` 407,485 条 / `val.jsonl` 19,925 条（约 1,400 万 token，字符 p50 131 / p99 301）；来源 MultiWOZ 38%、oasst assistant 50%、oasst prompter 12%（prompter 仅 2.2 万子句，×6 变体）；照抄 31.3% + 新词照抄 8.9%、邻键 17.0%、多错误 14.1%、真实错拼 5.5%、漏首字母 / 漏空格 / 空格错位 / 多空格 / 真词各约 4–5%；去泄漏丢弃 79,193 子句；LibriSpeech 默认不用（全小写会教坏专名大小写）。
  - **P1 数据集 v2（2026-11-23，加入学术文本）**：用户批准 arXiv 元数据后，`build_l3_sft.py` 新增 `arxiv` 来源（task-14 `data/compiled/arxiv_cs/train.jsonl`，子句上限 15 万，上文为同一摘要前一子句；指向 `heldout.jsonl` 时拒绝运行），`--extra-eval-tsv` 把尚未合入本分支的 `ood_academic_v1.tsv` 与 task-14 复核表加入去泄漏参照。重建结果：`train.jsonl` 552,108 条 / `val.jsonl` 20,000 条（128 MB）；来源 oasst assistant 37.4%、MultiWOZ 28.1%、**arXiv 25.3%**、oasst prompter 9.2%；照抄 30.2% + 新词照抄 12.5%（arXiv 术语多，新词照抄占其 23%）、多错误 13.6%；去泄漏丢弃 80,968 子句。上传包已刷新（train.jsonl sha256 前缀 3863fb5d）。
  - **P2 第一轮 run1（T4，2026-11-23）**：2,157 步 / 3.5 h / 约 4.5 计算点，loss 0.53 -> 0.08、eval_loss 0.074。首次导出的 GGUF 与基座逐字节相同——根因是本脚本 `export_base_tokenizer` 遍历拷贝了整个 HF 缓存快照目录，把基座 `model.safetensors` 覆盖到导出目录（**不是** transformers 5.x 的问题；此前误判已更正）。已修复为文件名白名单 + 存档 / 最终保存 / 转换前三处 `verify_weights_trained`；从 Drive 上未受影响的 `checkpoint-2157` 用 `--export-only --export-from` 重新导出（norm 权重 max|diff| ≈ 1e-2），`anime-l3-360m-v1.Q4_K_M.gguf` sha256 0573818a…、Q8_0 70b985ca…。另锁定 transformers==4.57.6（脚本按 4.x 验证）。
  - **融合修复**：纠错模型对正确答案给出约 0 的对数概率、对乱改候选 -20 ~ -40，均值中心化使"正确改写"与"照抄"同时截断到 +4.5 打平；`Correction` 风格改用以最优分为锚点的 `normalize_rerank_scores_top`（noise_v2 前 400 行 75.0 -> 83.2）。纠错模型只在整句上训练，单 token 输入（词补全 / 单词纠错）跳过 L3（core_v2 prefix 60.0 -> 82.9）。
  - **run1 评测**（引擎含 task-15 原样候选，去泄漏 L2，top1 % (95% CI)）：

    | 集合 | L1+L2 | + 通用 360M | **+ anime-l3 v1 Q4** |
    |---|---|---|---|
    | ood_dev_v1 TOTAL | 66.9 | 71.8 | **84.2 (82–86)** |
    | ood_dev negative:clean / novel | 88.7 / 50.0 | 90.7 / 62.7 | **100.0 / 100.0** |
    | ood_dev multi / space_shift / leading_drop | 32.0 / 58.3 / 53.3 | 32.0 / 66.7 / 61.7 | 44.0 / 88.3 / 75.0 |
    | ood_academic_v1 TOTAL | 59.3 | 63.0 | **76.2 (74–79)** |
    | ood_academic clean / novel | 88.0 / 70.7 | 91.7 / 67.3 | 99.7 / 100.0 |
    | ood_tm2_v1 TOTAL | 67.2 | 68.2 | **74.3 (71–78)** |
    | noise_v2 TOTAL | 78.7 | 79.5 | **83.3 (81–85)** |
    | core_v2 TOTAL（跳过单 token 后） | 83.1 | 83.6 | **72.4** |
    | realword_edit（dev / acad / noise_v2） | 61.7 / 43.3 / 73.3 | 73.3 / 60.0 / 84.0 | 58.3 / 51.7 / 62.7 |

    Q8_0 与 Q4_K_M 指标在 CI 内一致（ood_dev 84.0 / academic 76.2）；L3 推理 p50 约 100 ms（通用 82 ms），按键延迟 p99 <= 4.7 ms。
  - **v1 已知缺陷（需 v2 数据修复后重训）**：(1) 训练数据中**带错样本的键入原文一律被小写**，只有照抄样本保留大写 → 模型学到"原文含大写即照抄"，用户句首按 Shift 时不纠错（core_v2 `Do tbey have an entfance fee` 原样保留），core_v2 sentence 90.0 -> 62.5；(2) 真词纠错只有单编辑距离样本，缺 their/there/they're、your/you're、its/it's、to/too、then/than、form/from 等同音 / 易混词，core_v2 context_realword 95.0 -> 46.0、各集 realword_edit 下降。
  - **P2 第二轮 run2 = v2（T4，2026-11-23/24）**：数据 v2（带错样本 50% 保留原文大小写、新增同音 / 易混真词互换 `confusable` 13k 条，共 554,345 / 20,074 条），2,166 步 / 2.5 h / 约 3 计算点，eval_loss 0.072；存档、最终保存、转换前三处权重校验全部通过。冒烟验证：`Do tbey have an entfance fee` 正确改写得分 -0.04（v1 照抄 -0.03 / 改写 -8.0）。Colab 合计约 11 计算点（余 189.2）。
  - **P3 全量评测**（引擎 = main 含 task-15 原样候选 + task-16 易混真词分差；去泄漏 L2 3fcbf1b9；top1 % (95% CI)）：

    | 集合 / 指标 | L1+L2 | + 通用 360M | + v1 | **+ v2 Q4** |
    |---|---|---|---|---|
    | ood_dev TOTAL | 66.9 | 71.8 | 84.1 | **83.8 (81–86)** |
    | ood_dev clean / novel | 88.7 / 50.0 | 90.7 / 62.7 | 100 / 100 | **100 / 100** |
    | ood_dev multi / space_shift / leading_drop | 32.0 / 58.3 / 53.3 | 32.0 / 66.7 / 61.7 | 44.0 / 88.3 / 75.0 | 43.0 / 88.3 / 76.7 |
    | ood_academic TOTAL | 59.3 | 63.0 | 76.2 | **75.8 (73–78)** |
    | ood_academic clean / novel | 88.0 / 70.7 | 91.7 / 67.3 | 99.7 / 100 | 99.7 / 99.3 |
    | ood_tm2 TOTAL | 67.2 | 68.2 | 74.3 | **74.3 (71–78)** |
    | noise_v2 TOTAL | 78.7 | 79.5 | 83.2 | **84.2 (82–86)** |
    | core_v2 TOTAL | 83.1 | 83.6 | 72.4 | **81.5 (79–83)** |
    | core_v2 sentence / context_realword | 89.0 / 93.5 | 90.0 / 95.0 | 62.5 / 46.0 | **94.0 / 77.5** |
    | realword_edit（dev / acad / noise_v2） | 61.7 / 43.3 / 73.3 | 73.3 / 60.0 / 84.0 | 56.7 / 51.7 / 62.0 | 53.3 / 53.3 / 67.3 |

    Q8_0 与 Q4_K_M 在 CI 内一致（ood_dev 83.8 / academic 76.0）。按键延迟 p50 0.4–0.6 ms、p99 <= 4.4 ms；单次 max 偶发 22.5 ms（同一批次 L1+L2 15.2 ms、通用 17.0 ms，均为 L3 并发推理时的抖动，非本改动引入，记录待查）。L3 推理 p50 124 ms（通用 87 ms）。
  - **验收对照**：ood_dev TOTAL >= 72 ✅（83.8）、novel >= 60 ✅（100）、clean 不低于 L1+L2 ✅；ood_academic >= 66 ✅（75.8）、novel >= 60 ✅；ood_tm2 >= 74 ✅（74.3）；noise_v2 不退化 ✅（+4.7）；Q4_K_M <= 300 MB ✅（270.6 MB）。**未达标**：core_v2 TOTAL 81.5 < 通用 83.6（`context_realword` 77.5 vs 95.0：带上文的单词级真词纠错仍偏向照抄；`realword_edit` 各集低于通用模型）；L3 推理 p50 124 ms > 通用 87 ms（整句候选打分，GPU 优化按用户意见另行处理）。
  - 部署：本机 `~/.local/bin/animed`（task-07 + main）当前加载 v1（systemd drop-in `animed.service.d/l3-correction.conf` 指定 `ANIME_NEURAL_MODEL`）；是否切换 v2 由用户决定。
  - Rust 与 Python 模板黄金测试逐字节一致；`make check`（0 warning）/ `make test`（69 passed）/ `./scripts/verify-boundaries.sh` BOUNDARY OK。
