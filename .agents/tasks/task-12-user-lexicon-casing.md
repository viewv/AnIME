---
task_id: "task-12-user-lexicon-casing"
worktree: "../wt-anime-user-casing"
allowed_paths:
  - "crates/lexicon/src/**"
  - "crates/lexicon/tests/**"
  - "crates/lm-ngram/src/**"
  - "crates/engine/src/**"
  - "crates/engine/tests/**"
  - "crates/animed/src/**"
  - ".agents/tasks/task-12-user-lexicon-casing.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
  - "crates/lm-neural/**"
  - "crates/eval/**"
  - "eval/**"
  - "scripts/**"
  - "data/MANIFEST.yaml"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/data-format.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-12-user-lexicon-casing.md"
---

## Objective (目标)
治理 `user.db` (`user_words`) 与 L1 动态热词层（`LayeredLexicon` Layer 3）准入门槛过低及大小写互斥污染问题：解决偶发 Shift 粘连导致 `So → SO` 全大写锁死的问题，同时支持同一单词的大小写双词形共存（如 `Linux` / `linux`、`macOS` / `macos`），确保用户显式输入的大小写意图不被覆写。

---

## Background & Root Cause (问题现象与根因分析)

### 1. 真机复现现象：`So I think the local` 候选全部变成 `SO I think the local`
用户在输入框键入 `So I think the local`（首字母 `S` 大写、第 2 字母 `o` 已松开 Shift 为小写），候选栏 1~6 项全部出现 `SO`（两个字母全大写）：
`1. SO I think the local` / `2. SO I think the locality` / `3. SO I think the locale` ...

### 2. 串联根因拆解
1. **单次手滑零门槛入库**：
   - 用户在句首快打 `So` 时偶发一次 Shift 粘连（按住 Shift 敲完 `S` 后未及时松开便敲了 `o`），上屏了 1 次 `"SO"`；
   - 当前每次上屏（含 `Enter` 原串直出、单次错拼/手滑）均无门槛（`freq = 1`）写入 SQLite `user_words` 并立即通过 `teach_lexicon_commit` → `LayeredLexicon::learn_word(tok, 1)` 注入 L1 动态层（初始即赋予 `DEFAULT_DYNAMIC_BASE_ZIPF = 4.2`）。
2. **`user_words` 加载与合并时按小写归并，且按主键 ASCII 序让 `"SO"` 吞并 `"so"` 的全部频次**：
   - `crates/lm-ngram/src/user_db.rs` 的 `load_initial_memory` 执行 `SELECT word, freq, last_used FROM user_words;`；
   - SQLite 按 `word TEXT PRIMARY KEY` 的 ASCII 顺序返回：`"SO"` (`0x53 0x4F`) 排在 `"So"` (`0x53 0x6F`) 与 `"so"` (`0x73 0x6F`) 之前；
   - 内存中 `mem.words` 以小写 `"so"` 为 key 归并：先读入的 `"SO"`（哪怕 `freq = 1`）占住 `existing.word = "SO"`，随后读入的 `"So"` / `"so"`（累计几十次频次）被全部累加到 `"SO"` 名下！
   - 运行时 `record_commit` 同样存在 `if !entry.word.bytes().any(|b| b.is_ascii_uppercase()) && tok_has_upper` 无条件将已有高频小写词重命名为大写词形的问题。
3. **L1 动态层把 2 字母全大写 `"SO"` 误判为词内大写专名（`has_internal_upper`）并覆盖主词典**：
   - `crates/lexicon/src/layered.rs` `apply_learn_locked` 通过 `trimmed.chars().skip(1).any(|c| c.is_ascii_uppercase())` 判断是否为 `TypeScript` / `iPhone` 类混合大小写专名；
   - `"SO"` 的第 2 个字符 `'O'` 是大写，触发 `has_internal_upper = true`，作为 `is_proper_or_mixed = true` 的专名存入动态层；
   - 当累计 `freq >= 5` 时，`user_freq_to_zipf(freq) >= 5.71` 超过主词典 `"so"` 的基准词频（`zipf = 5.52, score = 4.02`）。
4. **候选去重按 `to_ascii_lowercase()` 互斥，且 `apply_input_casing` 无视用户已敲的小写字母**：
   - `apply_input_casing("So", "SO", is_proper_or_mixed=true)` 直接返回 `"SO"`，无视用户第 2 个字母显式敲的是小写 `'o'`；
   - `LayeredLexicon::merge_candidate` 以 `cand.word.to_ascii_lowercase()` 为 key 互斥覆盖，动态层更高分的 `"SO"` 将主词典的 `"So"` 彻底抹掉。

### 3. 为什么不能简单按 `has_lowercase_canonical` 一刀切强制转小写？
检查 `data/compiled/lexicon.tsv` 发现，因 `wordfreq` 词频源为全小写且 SCOWL 未收录部分现代技术词汇，主词典中许多常见双态词仅有纯小写词条且词频较高：
- `linux` (`zipf = 3.99, log_freq = -5.01, flags = 0`)
- `windows` (`zipf = 4.72, flags = 4`)、`apple` (`zipf = 4.76, flags = 4`)
- `python` (`zipf = 3.78, flags = 4`)、`rust` (`zipf = 3.74, flags = 4`)、`git` (`zipf = 3.36, flags = 4`)
- `macos` (`zipf = 3.02, log_freq = -5.98, flags = 0`)、`typescript` (`zipf = 2.59, flags = 4`)

在开发者日常输入中，这类词**大小写都有真实需求**（写文档/聊天时用 `Linux`、`macOS`，敲命令/代码标识时用 `linux`、`macos`）：
- 若按 `log_freq >= -5.2` 一刀切强制归档为小写，用户永远无法习得 `Linux`、`Python`、`Rust`；
- 若按小写 key 互斥覆盖，一旦习得 `Linux` / `macOS`，候选列表中又选不到小写的 `linux` / `macos`。

---

## Proposed Solution (解决方案设计)

### 1. `user_words` 精确词形独立计频与 L1 动态层晋升门槛（防单次手滑污染）
1. **精确词形独立统计，不再跨大小写吞并频次**：
   - `UserStore` 内存表按精确词形（或每个小写 key 下维护各精确大小写变体 `word_form -> freq`）分别统计频次：`"so": 50`、`"So": 20`、`"SO": 1` 各自独立计频，绝不把 `"so"` 的 50 次累加到 `"SO"` 的 1 次上；
   - L2 `word_boost` 查询时按大小写不敏感汇总或取对应词形频次，保持 L2 统计平滑。
2. **提高进入 L1 动态热词层（Layer 3）的准入门槛**：
   - **频次门槛（`MIN_DYNAMIC_PROMOTE_FREQ = 3`）**：通过打字自然上屏学习（`learn_word` / `preload_dynamic_words`）的词形，必须累计确认达到 `freq >= 3` 才晋升构建进 L1 动态层 `CompactTrie`；单次手滑（如 `freq = 1` 的 `"SO"` 或偶发拼错）只停留在 `user_words` 计数观察，不进入 L1 词典；
   - **显式 API 不受影响**：编程调用 `upsert_dynamic_word` 仍立即生效；
   - **`Enter` 原串直出降权/隔离**：按 `Enter` 强制直出 raw preedit 时，若该 token 并非已知词，不直接作为 L1 标准词条晋升。
3. **句首常规大写归一化与 Shift 粘连防护**：
   - 在输入框起始（`context.before` 为空）或句首位置上屏时，若首词是主词典高频封闭功能词/普通词的句首 TitleCase（如 `"So"`、`"The"`、`"Now"`）或 2~3 字母偶发全大写 Shift 粘连（如 `"SO"`、`"TO"`、`"IN"`、`"THE"`），在学习入库前归一化为小写词形计数。

### 2. 尊重用户显式键入的大小写形状（Input Casing Intent Guard）
在 `apply_input_casing(raw_trimmed, canonical, is_proper_or_mixed)` 及词格候选生成中区分用户显式大小写意图：
1. **全小写输入（`raw = "linux"` / `"macos"` / `"so"`）**：
   - 用户未按 Shift，既可匹配普通小写词（`"linux"`、`"macos"`、`"so"`），也可召回专名/混合大小写候选（`"Linux"`、`"macOS"`、`"iPhone"`）。
2. **首字母大写 + 后续含小写输入（TitleCase 输入，如 `raw = "So"` / `"Linux"`）**：
   - 用户在第 1 个字母按下 Shift、第 2 个字母已明确松开 Shift 敲出小写：
   - **严禁**被全大写词形（`"SO"`、`"LINUX"`）覆写！若候选词形是全大写（`canonical.chars().all(|c| !c.is_ascii_alphabetic() || c.is_ascii_uppercase())`），对 TitleCase 输入必须按 TitleCase 格式化为 `"So"` / `"Linux"` 或直接过滤掉全大写专名。
3. **全大写输入（`raw = "SO"` / `"CPU"`） 或 显式混合大写输入（`raw = "macOS"`）**：
   - 按用户显式敲入的全大写或混合大小写输出。

### 3. 支持同一单词的大小写双词形共存（`Linux` + `linux`、`macOS` + `macos`）
1. **L1 动态层与候选合并支持双态共存**：
   - 当主词典存在小写基础词（如 `linux`、`macos`、`rust`），且用户通过多次上屏（`freq >= 3`）习得了带大写特征的变体（如 `Linux`、`macOS`、`Rust`）时：
   - `LayeredLexicon` 动态层保留该大写变体，且在 `LayeredLexicon::query` 合并候选时，**不抹除主词典原本的小写词形**（去重 key 区分纯小写与专名大小写，或允许同一词根最多保留「1 个纯小写 + 1 个专名/混合大小写」两个精确候选）；
   - 动态层对主词典已收录高频词的分数加成设合理上限（不无限拔高到 `zipf = 7.2` 碾压基础词），使 `Linux` 与 `linux`（或 `macOS` 与 `macos`）在用户全小写输入 `linux` / `macos` 时同时出现在候选栏前排，并可由上文语境（L2/L3）或数字键自由选择。

---

## Verification Plan (验收标准)
1. **回归单测（`So → SO` 根治）**：
   - 模拟历史 `user.db` 中已有 `("SO", 1)`、40 次 `"so"`、15 次 `"So"`，或连续调用 `learn_word("SO", 1..10)`：
   - 输入 `"So"` 或 `"So I think the local"` 时，首选及整句候选首词必须为 `"So"`，绝不出现 `"SO"`；
   - 输入 `"so"` 时首选为 `"so"`；仅在用户显式输入全大写 `"SO"` 时才输出 `"SO"`。
2. **双态词共存单测（`Linux`/`linux`、`macOS`/`macos`）**：
   - 用户上屏 `"Linux"` 或 `"macOS"` 仅 1 次（`freq = 1`）时，未达晋升阈值，不污染 L1 词典；
   - 用户累计上屏 `"Linux"` 与 `"macOS"` 达阈值（`freq >= 3`）后，全小写输入 `"linux"` 时候选列表同时包含 `"Linux"` 与 `"linux"`；全小写输入 `"macos"` 时候选列表同时包含 `"macOS"` 与 `"macos"`。
3. **全量门禁**：
   - `make check`、`make test`、`make eval`、`make verify-infra` 及 `./scripts/verify-boundaries.sh .agents/tasks/task-12-user-lexicon-casing.md` 全部通过，评测集准确率与单键延迟不退化。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 核心 crate（`protocol` / `lexicon` / `lm-ngram` / `lm-neural` / `engine`）严禁引入 Linux/Fcitx5/macOS 平台专有依赖。
- 禁止无意义流水账注释；保留与键盘物理距离、读音规则、协议不变量、延迟预算相关的 Why 注释。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
