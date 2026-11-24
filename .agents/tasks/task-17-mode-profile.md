---
task_id: "task-17-mode-profile"
worktree: "../wt-anime-mode-profile"
allowed_paths:
  - "crates/engine/**"
  - "crates/eval/**"
  - ".agents/tasks/task-17-mode-profile.md"
  - "ROADMAP.md"
  - "Makefile"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
  - "crates/lm-neural/**"
  - "crates/animed/**"
  - "eval/**"
  - "scripts/**"
  - "data/MANIFEST.yaml"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-17-mode-profile.md"
---

## Objective (目标)
为单词模式（`space_commits = true`，空格上屏首选）与整句模式（`space_commits = false`，空格留在预编辑区）建立**行为中立**的分模式参数脚手架：引擎内一个 `ModeProfile` 收拢两种模式可能各自调参的全部打分 / 召回参数，按 `EngineConfig.space_commits` 选择，运行时切换立即生效；两份 profile 初值与现状完全相同，5 个冻结评测集逐行 Top-1 不变。评测器增加 `--mode word|sentence`。

## Background
- 实测两种模式瓶颈不同：整句模式受召回限制（top5 差 13–20 点），单词模式受排序限制（core_v2 top1 81.5 vs top5 96.3）。当前两者共用一套常量，调一边会伤另一边。
- 用户批准的 3 步计划：(1) 本任务——中立脚手架；(2) 单词模式调参轨道；(3) 整句模式召回轨道。(2)(3) 各自只改自己的 profile，可并行。
- 单词模式也有类整句路径：无空格连打（`iwanttogo`）在单 token 内切分，因此其 profile 也必须覆盖连打 / 词格参数。
- `space_commits` 已由协议 `config` 消息下发，不改 `crates/protocol` 与任何契约。
- L1 内部权重（`EXACT_BONUS`、`COMPLETION_*`、`EDIT_LAMBDA`，`crates/lexicon`）与 L2 词格内部权重（`LEX_LAMBDA` / `LM_LAMBDA` / `MAX_CANDS_PER_SLOT`，`crates/lm-ngram`）不在本任务路径内：profile 以引擎侧等价旋钮（按匹配类型的分数偏移、上下文 LM 缩放、词格词法分缩放、每槽候选上限）覆盖，默认值为恒等变换。

## 核心子任务
1. `crates/engine/src/profile.rs`：`ModeProfile`（`WORD` / `SENTENCE` 两个常量 + `for_space_commits`），收拢：L1 查询（上限、最大编辑代价）与按匹配类型偏移（exact / completion / typo / phonetic）、L2 上下文权重缩放、易混词（简写 / 真词近邻 / 频差补偿 / 邻键下限 3.0）、`SPACE_EDIT_PENALTY` 与备选切分数、`RUNON_*` 与连打候选分、每槽候选上限与读音保底窗口、词格 n-best 与词法分缩放、`VERBATIM_*`、部分选词、L3 请求开关。
2. `EngineSession` 每次重算候选时按 `config.space_commits` 取 profile（`set_config` 后立即生效）；提供 `with_profiles` / `set_profiles` 供调参与测试覆盖。
3. 单测：两份 profile 等于现状值；切换 `space_commits` 立即换 profile；自定义 profile 生效。
4. `anime-eval --mode word|sentence`：设置 `space_commits` 并按该模式回放（单词模式下空格上屏首选、累计已上屏文本参与排名）；`crates/eval/README.md` 写明各评测集推荐的运行方式；Makefile 增加分模式目标。
5. 中立性证据：5 个冻结评测集（L1+L2，无 `--model`）前后逐行 Top-1 文本完全相同；单键延迟守住 p50 <= 3 ms、p99 <= 10 ms、max <= 16 ms。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 核心 crate（`protocol` / `lexicon` / `lm-ngram` / `lm-neural` / `engine`）严禁引入 Linux/Fcitx5/macOS 平台专有依赖。
- 禁止无意义流水账注释；保留与键盘物理距离、读音规则、协议不变量、延迟预算相关的 Why 注释。
- 不合并 main、不推送、不重装 / 重启 live 守护进程（`~/.local/bin/animed`）。冻结评测集不得改动。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [DONE — 待评审] 已在 `feat/task-17-mode-profile` 本地提交；未合并 main、未推送、未重装 live 守护进程。
- 设计：
  - `crates/engine/src/profile.rs`：`ModeProfile` 9 组共 49 个字段（`lexicon` / `l2` / `confusables` / `space_edits` / `runon` / `lattice` / `verbatim` / `partial` / `l3`），`BASELINE_PROFILE` 逐值搬迁原常量，`WORD_PROFILE = SENTENCE_PROFILE = BASELINE_PROFILE`。`ModeProfiles::for_space_commits` 选择；`EngineSession` 每次重算时读 `profile()`，因此 `set_config` 切换 `space_commits` 立即生效（同时清空依赖参数的槽位 / 近邻 / L1 / 原样句缓存——此前 `set_config` 不清槽位缓存）。`set_profiles` / `ModeProfiles::apply_override("word.group.field=value")` 供调参覆盖；字段表用宏只写一次，同时生成设置、列出与键名清单。
  - 已收拢：L1 查询上限与最大编辑代价、按匹配类型偏移（exact / completion / typo / phonetic，默认 0，非零时才重排）、单 token 路径 L2 上下文权重缩放（默认 ×1.0）、易混词全部分差（简写 0.8、顺风 0.35×clamp 2.0、减免 0.22×clamp 1.8、下限 0.88、task-16 邻键单 token 下限 3.0、真词近邻 2.8/1.0/0.35/-6.0、非词余量 0.2、近邻 6 / 12、易混查询 6、锚点 `EXACT_BONUS` 6.5）、`SPACE_EDIT_PENALTY` 0.75 / 备选 8 / 两段切分 2、`RUNON_*` 与 `should_expand_runon_token` 门槛（1.25 / 7 / -5.5）及单 token 连打候选分（+1.5 / 0.5 / -1.5 / -2.0）、每槽上限（默认不截断）/ 读音保底窗口 10 / 闭合补全 +2 / 词格 n-best / 词格词法分缩放、`VERBATIM_*`（开关、位 1、0.01、1.25）、部分选词（2 条、-1.0）、L3 请求（`enabled`、`min_tokens`）。
  - 不在本任务路径内、未收拢的参数：L1 内部 `EXACT_BONUS` / `EDIT_LAMBDA` / `COMPLETION_*`（`crates/lexicon`），词格 `LEX_LAMBDA` / `LM_LAMBDA` / `MAX_CANDS_PER_SLOT` / `BEAM_WIDTH` 与 `CONTEXT_LIFT_WEIGHT`（`crates/lm-ngram`），送入 L3 的候选数与融合权重（animed / `lm-neural`）。profile 以引擎侧等价旋钮覆盖（类型偏移、`l2.context_weight`、`lattice.lex_scale`）；若后续轨道必须直接改这些参数，需另开跨 crate 任务。
  - L3：新增 `EngineSession::wants_l3_rerank()`（`enable_neural` + profile `l3`，与后端无关），`is_l3_pending` = 它 && 后端非 noop，所以 animed 依据 `final_l3` 的调度自动遵循 profile；anime-eval 的 `--model` 路径同样以它为门。
- Changed Files:
  - `crates/engine/src/profile.rs`（新）；`crates/engine/src/lib.rs`（常量改为读 profile，`profile` / `profiles` / `set_profiles` / `wants_l3_rerank`，3 个新单测）；`crates/engine/src/confusables.rs`（常量改为 `ConfusableProfile` 参数，Why 注释保留）。
  - `crates/eval/src/cli.rs`、`main.rs`、`metrics.rs`：`--mode word|sentence`（设置 `space_commits`；单词模式空格 / 标点上屏首选，排名文本 = 已上屏文本 + 剩余候选）、`--profile MODE.KEY=VAL` / `--profile list`、`--dump-rows PATH`（逐行 rank / top1 / top5）；1 个新单测。`crates/eval/README.md`（新）：各评测集按哪种模式跑。
  - `Makefile`：`eval-word` / `eval-sentence` / `eval-modes`（`EVAL_LEX` / `EVAL_NGRAM` / `EVAL_SETS` / `EVAL_OUT` / `EVAL_PROFILE`）。`ROADMAP.md`：登记 Task 17。
- Verification Output:
  - `make check`：通过（`cargo check --workspace --all-targets` 0 warning）；`make test`：15 组 `test result: ok`，共 78 passed、0 failed（engine 25 passed：含 `default_profiles_match_baseline_for_both_modes`、`space_commits_switches_profile_immediately`（只把单词模式邻键下限设为 0：整句模式 `to type fast` 保持 fast，切到 `space_commits=true` 后同一预编辑立即变为 vast，切回即恢复）、`profile_offsets_and_l3_gate_take_effect`；profile 模块 2 个；eval 12 个含单词模式回放）；`make eval`（smoke）：TOTAL 28.6%，与 main 相同；`make verify-infra`：`INFRA OK`；`./scripts/verify-boundaries.sh .agents/tasks/task-17-mode-profile.md`：`BOUNDARY OK（已检查 9 个文件，零越界）`。
  - 资产：词典 `data/compiled/lexicon.animelex.bin`，去泄漏 L2 `~/.local/share/anime/ngram.animengr.bin`，`--group-by-tag`，无 `--model`。
  - **中立性（逐行）**：先只做评测改动 + `wants_l3_rerank`（不动打分），构建 "before" 二进制；再接入 profile 构建 "after"。5 个集合 × 2 种模式共 10 份 `--dump-rows`（每行 rank + top-5 全文）**逐字节相同**（core_v2 1375、noise_v2 1650、ood_dev_v1 1030、ood_academic_v1 1030、ood_tm2_v1 600 行）。另以纯 main（181be85）二进制对照整句模式：5 个集合的全部类别 / 标签准确率与 `--show-failures 100000` 完整失误列表（含 top1 实际值）去掉延迟列后完全相同。
  - 基线（两模式，top1 / top5）：整句模式 core_v2 83.1 / 96.3、noise_v2 78.7 / 87.1、ood_dev 66.9 / 86.3、ood_academic 59.3 / 78.9、ood_tm2 67.2 / 80.7（与 task-16 证据一致）；单词模式 core_v2 80.7 / 92.5（词级 5 类与整句模式逐行相同；context_realword 84.5 / 88.0、sentence 81.0 / 82.0）、noise_v2 45.9 / 47.0、ood_dev 51.1 / 52.6、ood_academic 46.2 / 48.3、ood_tm2 50.2 / 53.5。
  - 单键延迟（TOTAL key p50 / p99 / max µs，逐个串行运行）：整句模式 main → 本分支：core_v2 93/1222/3879 → 91/1038/3302；noise_v2 355/3879/8970 → 222/2178/7213；ood_dev 316/4122/23591 → 353/3960/13263；ood_academic 246/2474/15378 → 232/2158/10861；ood_tm2 212/2105/7333 → 215/2318/12612。单词模式（本分支）：15–21 / 357–415 / 943–1918。全部 p50 <= 3 ms、p99 <= 10 ms；本分支 max 全部 <= 16 ms（main 的 23.6 ms 单次峰值属于系统噪声，profile 每次重算只多一次约 250 字节的结构体拷贝）。
