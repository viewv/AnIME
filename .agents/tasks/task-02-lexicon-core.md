---
task_id: "task-02-lexicon-core"
worktree: "../wt-anime-lexicon"
allowed_paths:
  - "crates/lexicon/**"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
  - "crates/lm-ngram/**"
  - "crates/lm-neural/**"
  - "crates/engine/**"
  - "crates/animed/**"
  - "crates/eval/**"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/data-format.md"
  - "crates/protocol/src/lib.rs"
acceptance_cmd: "cargo test -p anime-lexicon && ./scripts/verify-boundaries.sh .agents/tasks/task-02-lexicon-core.md"
---

## Objective (目标)
在 `crates/lexicon` 中实现高性能 L1 英文词法与纠错内核：紧凑前缀树、QWERTY 物理键盘键距代价矩阵、加权模糊 Trie 束搜索（支持 `appie → apple`、`beutif → beautiful` 这类截断错拼补全）、Double Metaphone 重度读音倒排纠错（`numonia → pneumonia`）、常见人类错拼表、大小写与漏撇号缩写恢复（`im → I'm`），以及 `ANIMELEX` 二进制词典加载器。

## Constraints (局部硬约束)
- 严格保留并实现 `LexiconProvider` Trait，不破坏跨平台纯净度（零 Linux/macOS 专有依赖）。
- 保证词级同步查询延迟可控（单次常规查询目标 `< 1 ms`），并保留单测与微观延迟测试。
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约/接口时，立即暂停并在 `Blocked` 栏写明原因与路径。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
