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
- Status: [DONE]
- Changed Files:
  - `crates/protocol/src/lib.rs`
  - `crates/protocol/src/json.rs`
  - `crates/protocol/src/ipc.rs`
  - `crates/lm-ngram/src/lib.rs`
  - `crates/lm-ngram/src/format.rs`
  - `crates/lm-ngram/src/model.rs`
  - `crates/lm-ngram/src/user_db.rs`
  - `crates/lm-ngram/src/viterbi.rs`
  - `crates/engine/src/lib.rs`
  - `crates/engine/src/confusables.rs`
  - `crates/animed/src/main.rs`
  - `crates/animed/src/paths.rs`
  - `crates/animed/src/l3_scheduler.rs`
  - `crates/animed/src/server.rs`
  - `.agents/tasks/task-03-ngram-and-engine.md`
- Verification Output:
  - `cargo check --workspace`: 0 errors, 0 warnings
  - `cargo test -p anime-protocol -p anime-lm-ngram -p anime-engine -p animed`:
    - `anime-protocol`: 4 passed (RFC-8259 JSON, full 9-req/5-resp NDJSON round-trip, Phase 0 `pong`, 64 KiB & newline limits, surrogate escapes, `extract_seq_best_effort`)
    - `anime-lm-ngram`: 4 passed (`ANIMENGR` v1 roundtrip & corruption checks, Top-K 2nd-order Beam Viterbi disambiguation, SQLite WAL `user.db` persistence & privacy isolation)
    - `anime-engine`: 8 passed (core preedit/smart-space/partial-commit + sentence-initial capitalization, real-word confusables & texting shorthand with L2 LM, privacy blocking `Enter`/candidate learning, `space_commits`, `seq`/`revision` stale-L3 drop)
    - `animed`: 3 passed (`--stdio` all-command dispatch + malformed/oversized recovery + empty-line tolerance, Unix socket `0700`/`0600` permissions + closed-loop `user.db` & L1 Layer 3 learning, async L3 `revision=1` push & stale-`seq` drop)
  - `cargo test --workspace`: all 37 workspace tests across 7 crates passed (0 warnings)
  - `cargo run -p anime-eval -- eval/fixtures/smoke.tsv`: `smoke.tsv` baseline passed
  - `cargo run -p animed -- --version`: `animed 0.1.0 (protocol v1)`
  - `printf '{"type":"ping"}\n{"type":"shutdown"}\n' | cargo run -p animed -- --stdio`: `{"type":"pong","version":"0.1.0"}`
  - `./scripts/verify-boundaries.sh .agents/tasks/task-03-ngram-and-engine.md`: `BOUNDARY OK (task-03-ngram-and-engine): 已检查 14 个文件，零越界`
