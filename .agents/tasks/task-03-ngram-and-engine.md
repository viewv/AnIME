---
task_id: "task-03-ngram-and-engine"
worktree: "../wt-anime-engine"
allowed_paths:
  - "crates/protocol/**"
  - "crates/lm-ngram/**"
  - "crates/engine/**"
  - "crates/animed/**"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/lexicon/**"
  - "crates/lm-neural/**"
  - "crates/eval/**"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/ipc-protocol.md"
  - ".agents/contracts/data-format.md"
  - "crates/lexicon/src/lib.rs"
  - "crates/lm-neural/src/lib.rs"
acceptance_cmd: "cargo test -p anime-protocol -p anime-lm-ngram -p anime-engine -p animed && cargo run -p anime-eval -- eval/fixtures/smoke.tsv && ./scripts/verify-boundaries.sh .agents/tasks/task-03-ngram-and-engine.md"
---

## Objective (目标)
完成 NDJSON IPC 完整编解码（`crates/protocol`）、L2 词格 Viterbi 解码 + `ANIMENGR` 2/3-gram + SQLite `user.db` 本机学习（`crates/lm-ngram`）、完整会话状态机（部分选词、智能尾空格与标点自动吞空格、多路候选融合排序与隐私保护，位于 `crates/engine`），以及 `crates/animed` Unix Domain Socket 常驻守护进程与异步 L3 调度骨架。

## Constraints (局部硬约束)
- `crates/protocol`、`crates/lm-ngram`、`crates/engine` 严禁依赖 Linux/macOS 平台 API；仅 `crates/animed` 可使用 Unix Socket 与 OS 运行时目录发现（遵循 `ANIME_DATA_DIR` / `XDG_*`）。
- 严格遵循 `ipc-protocol.md` 的 `seq` / `revision` 单调不变量与单行 `<= 64 KiB` 约束，并在 `animed` 验证 Unix Socket `0700`/`0600` 权限与 `--stdio` 模式。
- 仅改动 `allowed_paths` 内文件；必须跨界或改契约时，立即暂停并在 `Blocked` 栏申请。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
