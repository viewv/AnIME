---
task_id: "task-09-sentence-recall"
worktree: "../wt-anime-recall"
allowed_paths:
  - "crates/lexicon/src/**"
  - "crates/lexicon/tests/**"
  - "crates/engine/src/**"
  - "crates/engine/tests/**"
  - "crates/lm-ngram/src/viterbi.rs"
  - "crates/lm-ngram/src/lib.rs"
  - "crates/animed/src/server.rs"
  - "crates/eval/src/main.rs"
  - ".agents/tasks/task-09-sentence-recall.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
  - "crates/lm-neural/**"
  - "eval/**"
  - "scripts/**"
  - "data/MANIFEST.yaml"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-09-sentence-recall.md"
---

## Objective (目标)
补齐整句模式的 L1 召回缺口：已闭合 token 放开短词编辑与漏首字母、为真词提供按键近邻候选，并把空格视为可能出错的字符（多打 / 错位 / 漏打）在区间词格中统一解码，使正确整句能进入 L2/L3 的候选。防误纠由信道代价 + L2 打分承担，而不是靠拒绝召回。

> 叠加在 `feat/task-08-eval-and-l2-rebuild` 之上开发（依赖其 `noise_v1`、多参考评测与新 L2）。合入顺序：task-08 → task-09。

## Background (现状证据，task-08 最终 L2 = `ngram_kn_r20`)
`noise_v1` top1：`leading_drop` 8.3%、`space_extra` 0.0%、`boundary_gemination` 26.7%、`space_shift` 40.0%、`space_missing` 45.0%、`realword_edit` 38.3%；`negative:clean` 95.5%。`core_v2` sentence top1 88.0%、TOTAL 82.8%。

根因（task-08 期间定位）：
1. `crates/lexicon/src/trie.rs` `fuzzy_search_with_width`：单字母 token 编辑预算为 0、2 字母上限 0.65；首字母漏打（`i == 0` 的删除扩展）仅在 `n >= 5` 时允许 → `o` 召不回 `to`、`ou` 召不回 `you`。
2. `crates/lexicon/src/lexicon.rs` `query_layer`：精确命中真词走轻量快路径，长度 < 3 不做任何纠错，>= 3 也只做 0.75 预算邻居；易混词扩展（`crates/engine/src/confusables.rs`）只基于 CMUdict 读音 → `he/the`、`hat/that`、`is/this`、`wan/want` 无候选。
3. 空格是硬分隔：`segment_runon` 只处理漏空格方向；多打空格（`pr ice`）、空格错位（`wantt o`、`theatrey ou`）没有任何路径。

## 核心子任务
### P1 闭合 token 召回（不改词格结构）
1. **闭合词近邻召回**：为"已被空格闭合"的 token 提供独立召回入口（如在 `LexiconProvider` 增加带默认实现的方法），对 token 枚举一次编辑（含首位插入 / 删除 / QWERTY 邻键替换 / 相邻换位）并直接查词典哈希，只保留常见词；代价沿用 `keyboard.rs` 物理距离模型，写清 Why 注释。
2. **作用范围**：只在整句模式（`tokens.len() >= 2`）的闭合 token 上启用；正在输入的末尾前缀、单词模式行为保持不变。
3. **身份先验**：用户原样键入的词（尤其已是真词时）保留明确的保留奖励，使无语境证据时零误改；用 `negative:clean` 与 `core_v2` 的 `exact` 校准。
### P2 空格可出错的区间词格
1. 在 `crates/lm-ngram/src/viterbi.rs` 新增区间（DAG）解码：边可覆盖连续 1–2 个 token（合并，解决 `space_extra`）、在相邻 token 间移动 1 个字符（解决 `space_shift` / `boundary_gemination`），并输出多词边（复用 `segment_runon` 解决 `space_missing`）。保留现有线性 `decode_lattice` 以兼容。
2. 每种空格编辑带信道代价，默认路径（按用户空格切分）最优先；代价常量写 Why 注释。
3. `crates/engine/src/lib.rs` 改为构建区间边并调用新解码；`consumed_chars` / 部分选词语义不变（core-api §2）。

## 验收目标（L1+L2，最终 L2 `ngram_kn_r20`，`LM_LAMBDA` 可重调）
- `noise_v1`：`leading_drop` >= 60%、`boundary_gemination` >= 60%、`space_extra` >= 50%、`space_shift` >= 60%、`space_missing` >= 60%、`realword_edit` >= 45%（top1）。
- **零误伤**：`negative:clean` top1 >= 95.5%（不低于 task-08）；`core_v2` 各类 top1 退化 <= 0.5pp，`exact` >= 99%；`core_v1` 同样不退化。
- **延迟**：`core_v2` / `noise_v1` `--gate phase3` 通过（单键 p50 <= 3 ms、p99 <= 10 ms、max <= 16 ms）。
- **防过拟合**：`noise_v1` 是开发集；验收前另用 `build_eval_v2.py --seed <其他值> --out-noise <scratch 路径>` 生成一份留出噪声集（不入库）复测，指标与开发集差距在 Evidence 说明。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 评测集（`eval/**`）与评测器（`crates/eval/**`）在本任务冻结，不得为提分修改；如发现标注错误，写入 Blocked 申请。
- 核心 crate 不引入平台专有依赖；保留键盘物理距离、信道代价、延迟预算的 Why 注释，禁止流水账注释。
- 不改 `protocol` / IPC 语义；单词模式与前缀补全行为保持不变（由 `core_v2` 的 prefix / typo_* 类别守护）。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- **[已批准 2026-11-22] 新增 trait 方法需在两个包装器中转发**：P1 在 `LexiconProvider` 新增带默认空实现的 `closed_token_neighbors`；`crates/animed/src/server.rs`（`SharedLexicon`）与 `crates/eval/src/main.rs`（`EvalLexicon`）各自包了一层且未转发，真实守护进程与评测器都拿到空默认实现（实测指标零变化）。用户批准**仅**在这两个文件各加一个与 `segment_runon` 相同模式的转发方法，已加入 `allowed_paths`；`crates/animed`、`crates/eval` 其余文件仍不在白名单内。

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
