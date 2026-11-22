# AnIME 开发路线图

## Phase 0：工程基建与可编译骨架（当前已完成）

- 按 Agent 工程化体系建仓：`AGENTS.md`、`.agents/map.yaml`、4 份锁定契约（`core-api` / `ipc-protocol` / `data-format` / `eval-schema`）、任务单模板与 Phase 1 任务单、`scripts/verify-boundaries.sh`、`scripts/agent-wt.sh`。
- 搭建平台无关 Rust Workspace 7 个 crate 的接口骨架与零外部依赖烟雾实现，`cargo check` / `cargo test` 离线通过。
- 搭建 Fcitx5 C++20 薄前端可编译骨架（`libanime.so`）与 `eval/fixtures/smoke.tsv` 7 类评测基准框。

## Phase 1：开源数据管线 + L1/L2 核心引擎 + Wayland Fcitx5 MVP（可并行派单）

对应 `.agents/tasks/task-01` ~ `task-04`，支持在独立 `git worktree` 中按契约并行实现：
1. **Task 01（数据与评测）**：编写可复现开源词表/错拼/词频构建脚本（SCOWL + `wordfreq` + Wikipedia 常见错拼），扩充 7 类场景评测集（覆盖重度读音错拼、截断前缀纠错、真词歧义与整句）及指标回归门禁。
2. **Task 02（L1 词法纠错引擎）**：实现紧凑词典 Trie、QWERTY 物理键距矩阵、加权模糊 Trie 束搜索（`appie → apple`、`beutif → beautiful`）、Double Metaphone 读音倒排（`numonia → pneumonia`）、大小写与缩写恢复（`im → I'm`）。
3. **Task 03（IPC / L2 整句与引擎核心 / `animed`）**：实现完整 NDJSON 编解码、词格 Viterbi + 2/3-gram + SQLite 个人学习、部分选词与智能标点回删、Unix Socket 常驻守护进程。
4. **Task 04（Fcitx5 Wayland 前端）**：基于异步 Unix Socket 客户端与 `seq`/`revision` 防过期机制，接入预编辑、数字键选词、`Enter` 原串直出、`Shift` 直通、surrounding text 与隐私框保护。

## Phase 2：L3 本地神经整句精排、数据驱动底座与连打分词（当前已完成）

- **Task 05（L3 神经层接入）**：为 `crates/lm-neural` 接入真实端侧小模型推理（`SmolLM2-135M / 360M` Q4_K_M GGUF，支持 CPU 与 Radeon 780M Vulkan GPU 加速），基于光标前文实现异步候选重排与下一词/短语续写预测（`✦` 标记），严格遵守 `seq`/`revision` 不抢键约定。
- **Task 06（引擎底座修补、CMUdict 数据驱动易混词与智能连打分词）**：消除 L3 子词长度归一化偏差，实现句内共享词前缀 KV 折叠；引入开源 CMUdict 构建 100% 数据驱动易混真词索引（移除手写词表）；实现支持带错切分的智能连打分词（`segment_runon`）并修复真机交互 Bug。

## Phase 3：专用模型蒸馏微调、桌面自启打包、macOS IMKit 移植与公开发布

- **Task 08（评测修复与 L2 重建，`.agents/tasks/task-08-eval-and-l2-rebuild.md`，优先于 Task 07）**：评测引入多参考答案、`core_v2`、真实打字噪声集 `noise_v1` 与干净整句反例集；用全量 LibriSpeech + 对话领域洁净语料与修正 Kneser-Ney 重建 `ANIMENGR`，并提供可复用的评测去泄漏过滤器。
- **Task 09（整句模式 L1 召回重构，`.agents/tasks/task-09-sentence-recall.md`，叠加在 Task 08 之上）**：闭合 token 放开短词编辑与漏首字母、真词按键近邻候选，空格作为可出错字符的区间词格（统一 `segment_runon`）。
- **Task 10（评测 v3，`.agents/tasks/task-10-eval-v3.md`，叠加在 Task 09 之上）**：打字过程交互指标、复核反例集、多错误噪声集 `noise_v2`、跨领域留出集与本机打字记录回放；日常/技术领域语料需用户批准下载。
- **Task 11（L2 语料扩展，`.agents/tasks/task-11-l2-corpus-expansion.md`）**：许可合规的日常与技术领域语料调研与接入。
- **Task 12（用户词典准入门槛与大小写双态共存，`.agents/tasks/task-12-user-lexicon-casing.md`）**：治理 `user_words` 与 L1 动态层低门槛入库及大小写互斥覆盖问题（根治 `So → SO` 粘连污染，支持 `Linux`/`linux`、`macOS`/`macos` 等大小写双词形共存与输入大小写意图保护）。
- **可挂载领域词典**：技术 / 开源专名词库作为独立可挂载词典层（复用 L1 分层词典），不并入基础词库。
- **Task 07（端侧小模型蒸馏与重排微调，`.agents/tasks/task-07-neural-distillation.md`）**：使用公开语料、合成物理键盘/读音/连打噪声、L1+L2 词格难负例与 `SmolLM2-1.7B` 教师软标签，微调/蒸馏输入法专用的 `SmolLM2-135M / 360M` GGUF 模型，将整句与真词消歧准确率推向 `1.7B` 水平。
- 在不改动 Rust 核心的前提下实现 `frontends/macos/`（macOS IMKit `IMKInputController`）。
- 完善 Linux 守护进程自启（`systemd --user` / 前端按需拉起）、设置界面、跨发行版打包与发布文档，正式公开仓库。
