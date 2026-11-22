---
task_id: "task-07-neural-distillation"
worktree: "../wt-anime-neural-distillation"
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
  - "frontends/macos/**"
  - "frontends/fcitx5/**"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
  - "crates/engine/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/data-format.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "make check && make test && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-07-neural-distillation.md"
---

## Objective (目标)
构建 100% 开源洁净的输入法候选重排与续写蒸馏/微调训练管线（基于 `SmolLM2-1.7B` 教师模型软标签 + 合成 QWERTY 邻键/读音/连打漏空格噪声 + 真实 L1/L2 词格难负例），微调端侧轻量模型（`SmolLM2-135M` / `SmolLM2-360M` LoRA $\rightarrow$ 合并导出 GGUF `Q4_K_M` / `Q8_0`），在不改动 `crates/lm-neural` 推理协议的前提下，将 `135M` / `360M` 的整句（`sentence`）与真词纠错（`context_realword`）准确率逼近 `1.7B` 教师模型水平（目标：`135M` 整句 Top-1 $\ge 77.5\%$ 且 Vulkan $p50 \le 30\text{ ms}$；或 `360M` 整句 Top-1 $\ge 80.0\%$ 且 Vulkan $p50 \le 45\text{ ms}$）。

### 核心子任务拆解
1. **难负例与教师软标签数据集构建（`scripts/data_pipeline/build_distill_dataset.py`）**：
   - **开源语料源**：从已登记的 OpenSLR LibriSpeech 语料与开源日常口语/技术文本中采样洁净英文句子，**严格剔除**与 `eval/benchmarks/core_v1.tsv`、`eval/fixtures/smoke.tsv` 及 OOD 测试集存在文本重叠（Exact / 5-gram 重叠）的句子，防止评测泄漏。
   - **合成输入法噪声与词格难负例挖掘**：
     - 对洁净句施加真实打字噪声（QWERTY 物理邻键替换、漏键/多敲/相邻转置、省撇号缩写 `whats/dont/youre/ill`、CMUdict 同音/近音真词替换 `there/their/they're`、`suite/sweet`、以及连打漏空格 `itwould/takeyearys/Ilikethe`）；
     - 调用 L1+L2 引擎生成 Top-$K$ 混淆候选列表 `[c_pos, c_neg_1, ..., c_neg_k]`，保留 L1+L2 首选错误或分差极小的**难负例（Hard Negatives）**。
   - **1.7B 教师模型蒸馏打分**：
     - 使用 `SmolLM2-1.7B` 对 `(shared_prefix, [c_pos, c_neg_1, ...])` 计算教师联合对数概率 $\log P_{\text{teacher}}(c_i \mid \text{prefix})$ 及短语续写目标，输出标准 JSONL 训练集与验证集。
2. **因果 LM 重排对齐与微调脚本（`scripts/data_pipeline/train_ime_lm.py`）**：
   - 支持 LoRA / QLoRA 微调 `HuggingFaceTB/SmolLM2-135M` 与 `HuggingFaceTB/SmolLM2-360M`；
   - **联合损失函数**：
     $$\mathcal{L} = \mathcal{L}_{\text{NLL}}(c_{\text{pos}} \mid \text{prefix}) + \lambda_{\text{rank}} \sum_{c_{\text{neg}}} \max\bigl(0,\; \gamma - (\log P_\theta(c_{\text{pos}} \mid \text{prefix}) - \log P_\theta(c_{\text{neg}} \mid \text{prefix}))\bigr) + \lambda_{\text{KL}} \mathcal{D}_{\text{KL}}\bigl(P_{\text{teacher}} \parallel P_\theta\bigr)$$
     在不改变模型架构与 BPE 词表的前提下，专项拉大正确整句相对于同音真词（`suite` vs `sweet`）、口语缩写（`what's` vs `whats`、`let's` vs `lets`、`I'll` vs `ill`）和形近干扰词的联合 log-prob margin。
3. **GGUF 转换、量化与 `MANIFEST.yaml` 溯源登记（`scripts/data_pipeline/export_ime_gguf.py`）**：
   - 将 LoRA 适配器合并回基础权重，导出为 GGUF `F16` 并量化为 `Q4_K_M` / `Q8_0`；
   - 在 `data/MANIFEST.yaml` 登记微调脚本、超参数、基础模型许可证（`Apache-2.0`）与产出 GGUF SHA256。
4. **端到端离线评测与门禁验证**：
   - 使用 `./target/release/anime-eval` 在 `eval/benchmarks/core_v1.tsv`（1400 条）与 30 条 OOD 真词泛化集上对比微调前后 `135M` / `360M` / `1.7B` 的 7 类 Top-1/Top-3/Top-5 准确率与 Radeon 780M Vulkan 延迟，确保零退化且整句/真词指标显著提升。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- **100% 开源洁净数据与评测零泄漏红线**：训练语料严禁包含任何闭源/未登记来源数据，且必须通过哈希/n-gram 过滤与 `eval/` 下所有评测集 100% 物理隔离。
- 导出的 GGUF 模型必须与现有 `crates/lm-neural` (`LlamaNeuralModel`) 零代码改动直接兼容，保持 `unsafe_code = "forbid"`（除 `llama-backend` FFI 封装外核心 workspace 不变）与 `< 50 ms` 端侧异步预算。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
