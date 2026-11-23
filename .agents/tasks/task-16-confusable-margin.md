---
task_id: "task-16-confusable-margin"
worktree: "../wt-anime-confusable"
allowed_paths:
  - "crates/engine/**"
  - ".agents/tasks/task-16-confusable-margin.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
  - "crates/lm-neural/**"
  - "crates/animed/**"
  - "crates/eval/**"
  - "eval/**"
  - "scripts/**"
  - "data/MANIFEST.yaml"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-16-confusable-margin.md"
---

## Objective (目标)
单词模式（`SpaceCommits=True`）下，有上文时用户正确敲出的常见词不得被上下文易混真词轻易翻盘：修正 `crates/engine/src/confusables.rs::expand_slot_confusables` 的先验分差，使 `type fast` / `to type fast` 中 `fast` 稳居 Top-1，同时 `fill out the from -> form` 一类真词纠错（core_v2 `context_realword`）不回退。

## Background
- live 守护进程复现：输入 `fast`，上文 `type ` 时 `fast` -0.63 vs `vast` -1.34；上文 `to type ` 时 0.45 vs -0.15；无上文时 2.87 vs -4.99（无上文不展开易混词）。用户实际看到 `vast` 排第一。
- 机制：`has_prev_word_ctx` 时 `lexicon.contextual_confusables` 加入同音 / 近音 / 换位 / 邻键真词，分数 `anchor - effective_penalty`（anchor = 精确词 L1 分 = log_freq + 6.5）。易混词比原词罕见时 `deficit_relief` 把惩罚降到下限 0.88；随后 L2 `score_in_context` 的 bigram 加分对两者几乎无区分（`type fast` / `type vast` 都稀有）。
- `ConfusableCandidate` 只有 `word` / `log_freq` / `relative_penalty`，无来源标记（`crates/lexicon` 本任务只读）。
- 单词模式不走 L3（task-07 纠错模型跳过单 token），因此只评测 L1+L2。

## 核心子任务
1. 用证据定位翻盘原因（逐行看 `context_realword` 命中所需的最小分差），选择修复：按易混族类型区分下限，和 / 或要求易混词拿到明确的 L2 优势后才可超过精确词。
2. 单测：`fast` 在上文 `type ` / `to type ` 后保持 Top-1；`fill out the from -> form` 类测试继续通过。
3. 评测：`anime-eval --group-by-tag`（词典 `data/compiled/lexicon.animelex.bin`，去泄漏 L2 `~/.local/share/anime/ngram.animengr.bin`，无 `--model`）在 `core_v2`（`context_realword` 不得超出 CI 回退，`exact` / `typo_*` 同理）、`noise_v2`、`ood_dev_v1`、`ood_academic_v1`、`ood_tm2_v1`（`negative:clean` 不得下降）前后对比 + Wilson 95% CI；另用 scratch TSV（不入库）测"上文 + 精确常见词 + 邻键真词近邻"行。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 核心 crate（`protocol` / `lexicon` / `lm-ngram` / `lm-neural` / `engine`）严禁引入 Linux/Fcitx5/macOS 平台专有依赖。
- 不得重新安装或重启 live 守护进程（`~/.local/bin/animed`，systemd --user）除非用户明确同意；不合并 main、不推送。
- 冻结评测集不得改动。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [DONE — 待评审] 已在 `feat/task-16-confusable-margin` 本地提交；未合并 main、未推送、未重装 live 守护进程。
- 根因（逐分量核对）：单 token 路径的 L2 加分 `score_in_context` = 1.4 ×（log10 P(w|上文) − log10 P(w)），即**与词频无关的 PMI 提升**；词格路径（`viterbi.rs`）用的是完整条件概率 `log10_trigram`。`deficit_relief` 的设计前提"罕见易混词在 L2 中吃 unigram 亏"只在词格路径成立，在单 token 路径反而重复补偿：`vast`（log10 频 −4.51）比 `fast`（−3.87）罕见，惩罚由 1.40 降到 1.26，且 PMI 对罕见词偶见搭配给出更高提升（去泄漏 L2：`the vast` 比 `the fast` 高约 2.4，`that vast` 同理）。live 守护进程的 L3（`anime-l3-360m-v1`）对单 token 候选不改分（复测 revision 1 分数不变），翻盘完全来自 L1+L2。
- `ConfusableCandidate` 无来源标记（`crates/lexicon` 只读，未改）；`fast/vast` 在词库中来自 CMUdict 擦音清浊近音边（惩罚 1.40），与真同音词（their/there 1.32、to/too 1.25）惩罚区间重叠，无法按惩罚值区分。改用引擎内可判定的正交特征：**等长、仅一处替换、替换双方为 QWERTY 直接邻键（`anime_lexicon::keyboard::is_direct_neighbor`）且不同为元音**。词库内此类易混对很少（干净句词表 2085 词中 12 对：fast/vast、life/live、fine/vine、has/had、base/bass、reference/reverence、organization/organisation 等），元音互换（in/on、then/than）不受影响。
- 被否决的方案（scratch 网格，L1+L2；wm:hand / wm:clean / wm:realword / core_v2 context_realword Top-1 %）：
  - 基线：94.6 / 98.5 / 63.9 / 93.5；
  - L2 优势门槛 τ（易混词须比精确词多 τ 的 L2 提升才可领先）：τ <= 1.0 完全无效（翻盘本就要求 L2 差 > 惩罚 >= 1.25），τ=1.5：94.6 / 98.8 / 63.3 / 93.5，τ=2.0：— / 99.0 / 59.8 / 92.5，τ=3.0：— / 99.8 / 46.9 / 92.0；
  - 单 token 路径把 relief 换成对称的词频差附加惩罚 0.35×Δlog_freq：96.4 / 98.8 / 63.2 / 93.5（`that fast` 仍翻）；0.5–1.0：96.4–98.2 / 98.8–98.9 / 62.6–63.0 / **93.0**（丢 `me to -> too`）；
  - 全局下限 1.6–1.8：94.6–96.4 / 98.8 / 62.8–63.2 / 93.5；
  - **采用：仅单 token 路径的邻键误触对惩罚下限 3.0**：100.0 / 98.5 / 63.6 / 93.5（下限 2.5 时 `the fast` 余量仅 0.11，3.0 时 0.61，多付 1 行 scratch realword）。
- Changed Files:
  - `crates/engine/src/confusables.rs`：`expand_slot_confusables` 新增 `single_token` 参数；单 token 路径下邻键误触真词对的有效惩罚取 `max(原惩罚, ADJACENT_KEY_SINGLE_TOKEN_MIN_PENALTY = 3.0)`；新增 `is_adjacent_key_substitution`。整句词格路径与原惩罚计算不变。
  - `crates/engine/src/lib.rs`：3 处调用传入 `single_token`（单 token 分支 true，词格两处 false）；新单测 `word_mode_keeps_exact_word_against_adjacent_key_confusable`（上文 `type ` / `to type ` / `the ` 后 `fast` 保持 Top-1 且 `vast` 仍在 Top-5，`fill out the from -> form` 仍翻盘；把下限改为 0 时该测试在 `to type ` 失败，已验证）。
  - `ROADMAP.md`：登记 Task 16。
- Verification Output:
  - `make check`：通过；`make test`：15 组 `test result: ok`，共 72 passed、0 failed（engine 20 passed）；`make eval`（smoke）：TOTAL 28.6%，与改动前相同；`make verify-infra`：`INFRA OK`；`./scripts/verify-boundaries.sh .agents/tasks/task-16-confusable-margin.md`：`BOUNDARY OK`。
  - 评测资产：词典 `data/compiled/lexicon.animelex.bin`（sha256 前缀 f0935298，与 live 相同），去泄漏 L2 `~/.local/share/anime/ngram.animengr.bin`（3fcbf1b9），`anime-eval --group-by-tag`，无 `--model`。"前" = main ad624dc，"后" = 本分支。
  - **冻结评测集：5 个集合的全部类别 / 标签 Top-1 与 Top-5 完全相同，逐行失误列表（`--show-failures`）逐字节相同**：

    | set | tag / category | n | 前 top1 (95% Wilson CI) / top5 | 后 top1 (95% Wilson CI) / top5 |
    |---|---|---|---|---|
    | core_v2 | exact | 200 | 99.0 (96.4–99.7) / 99.5 | 99.0 (96.4–99.7) / 99.5 |
    | core_v2 | prefix | 175 | 82.9 (76.6–87.8) / 99.4 | 82.9 (76.6–87.8) / 99.4 |
    | core_v2 | typo_simple | 200 | 92.5 (88.0–95.4) / 100.0 | 92.5 (88.0–95.4) / 100.0 |
    | core_v2 | typo_prefix | 200 | 56.0 (49.1–62.7) / 88.5 | 56.0 (49.1–62.7) / 88.5 |
    | core_v2 | typo_phonetic | 200 | 69.0 (62.3–75.0) / 91.0 | 69.0 (62.3–75.0) / 91.0 |
    | core_v2 | context_realword | 200 | 93.5 (89.2–96.2) / 99.5 | 93.5 (89.2–96.2) / 99.5 |
    | core_v2 | sentence | 200 | 89.0 (83.9–92.6) / 96.5 | 89.0 (83.9–92.6) / 96.5 |
    | core_v2 | TOTAL | 1375 | 83.1 (81.0–85.0) / 96.3 | 83.1 (81.0–85.0) / 96.3 |
    | noise_v2 | TOTAL | 1650 | 78.7 (76.7–80.6) / 87.1 | 78.7 (76.7–80.6) / 87.1 |
    | noise_v2 | negative:clean | 300 | 98.0 (95.7–99.1) / 100.0 | 98.0 (95.7–99.1) / 100.0 |
    | ood_dev_v1 | TOTAL | 1030 | 66.9 (64.0–69.7) / 86.3 | 66.9 (64.0–69.7) / 86.3 |
    | ood_dev_v1 | negative:clean | 300 | 88.7 (84.6–91.8) / 100.0 | 88.7 (84.6–91.8) / 100.0 |
    | ood_dev_v1 | negative:novel | 150 | 50.0 (42.1–57.9) / 100.0 | 50.0 (42.1–57.9) / 100.0 |
    | ood_academic_v1 | TOTAL | 1030 | 59.3 (56.3–62.3) / 78.9 | 59.3 (56.3–62.3) / 78.9 |
    | ood_academic_v1 | negative:clean | 300 | 88.0 (83.8–91.2) / 100.0 | 88.0 (83.8–91.2) / 100.0 |
    | ood_academic_v1 | negative:novel | 150 | 70.7 (63.0–77.4) / 100.0 | 70.7 (63.0–77.4) / 100.0 |
    | ood_tm2_v1 | TOTAL | 600 | 67.2 (63.3–70.8) / 80.7 | 67.2 (63.3–70.8) / 80.7 |
    | ood_tm2_v1 | negative:clean | 200 | 94.5 (90.4–96.9) / 100.0 | 94.5 (90.4–96.9) / 100.0 |
    | scratch wm | wm:hand | 56 | 94.6 (85.3–98.1) / 100.0 | 100.0 (93.6–100.0) / 100.0 |
    | scratch wm | wm:clean | 1546 | 98.5 (97.8–99.0) / 100.0 | 98.5 (97.8–99.0) / 100.0 |
    | scratch wm | wm:realword | 1528 | 63.9 (61.5–66.3) / 99.4 | 63.6 (61.2–66.0) / 99.4 |

    | set | noise:* 合并 n | 前 top1 (CI) | 后 top1 (CI) |
    |---|---|---|---|
    | noise_v2 | 1350 | 74.4 (72.0–76.6) | 74.4 (72.0–76.6) |
    | ood_dev_v1 | 580 | 60.0 (56.0–63.9) | 60.0 (56.0–63.9) |
    | ood_academic_v1 | 580 | 41.6 (37.6–45.6) | 41.6 (37.6–45.6) |
    | ood_tm2_v1 | 400 | 53.5 (48.6–58.3) | 53.5 (48.6–58.3) |

  - **scratch 单词模式集**（未入库，副本 `/home/viewv/.config/Claude/side-session-notes/local_5746fbc7-896b-4007-a6bb-ad5aca31c65e/task-16-wm_scratch.tsv`）：从 4 个集合的 `negative:clean` 句子中，对位置 >= 1 且有易混词的词生成"上文 + 原词 -> 原词"（wm:clean，每句至多 2 处）与"上文 + 首个易混词 -> 原词"（wm:realword，模拟真词错误）；另加 56 行手写行（wm:hand：fast/vast、cast、bat、fill、form、fix 等，在 `type ` / `to type ` / `the ` / `that ` / `the most ` 等上文后）。逐行变化：修复 3 行（`the` / `that` / `the most` + `fast`）；wm:realword 丢 4 行（`problems in live -> life`、`better organisation -> organization`、`a reverence -> reference`、`and the reverence -> reference`），四行的正确答案均仍在 Top-5（top5 99.4 不变），且都是"用户敲出的合法真词被保留"，符合优先级。
  - **live 复现（本分支 `animed --stdio`，`ANIME_DATA_DIR` 指向复制的 user.db，未触碰 live 进程）**，输入 `fast`：

    | 上文 | live 守护进程 | 本分支 |
    |---|---|---|
    | `type ` | fast -0.49 / vast -1.26 | fast -0.49 / vast -3.00 |
    | `to type ` | fast 0.59 / vast -0.07 | fast 0.59 / vast -1.81 |
    | `the ` | **vast 4.00** / fast 2.87 | fast 2.87 / vast 2.26 |
    | `that ` | **vast 2.15** / fast 1.27 | fast 1.27 / vast 0.42 |
    | （无） | fast 3.01 / vast -4.65 | 不变 |

  - 单键延迟（TOTAL key p50 / p99 / max µs，5 集并行运行，前 → 后）：core_v2 143/2469/6252 → 133/2154/6172；noise_v2 408/4131/8333 → 390/3983/8697；ood_dev 593/7138/26156 → 551/6720/27881；ood_academic 450/4850/11757 → 422/4478/12197；ood_tm2 468/5153/13167 → 441/4740/14646。改动只是每个易混词一次 O(len) 比较，max 由并行负载噪声主导。
  - 后续（不在本任务路径内）：整句词格路径没有改动；单 token 路径对非邻键易混词仍保留 `deficit_relief`（与 PMI 加分叠加属于重复补偿，但改为对称附加惩罚会丢 `me to -> too` 等真同音纠错，需更大的单词模式评测集再定）。
