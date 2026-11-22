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
  - `crates/lm-ngram/README.md`
  - `crates/engine/src/lib.rs`
  - `crates/engine/src/confusables.rs`
  - `crates/engine/tests/real_l1_l2_eval.rs`
  - `crates/animed/src/main.rs`
  - `crates/animed/src/paths.rs`
  - `crates/animed/src/l3_scheduler.rs`
  - `crates/animed/src/server.rs`
  - `.agents/tasks/task-03-ngram-and-engine.md`
- Verification Output:
  - `cargo check --workspace`: 0 errors, 0 warnings
  - `cargo test -p anime-protocol -p anime-lm-ngram -p anime-engine -p animed`:
    - `anime-protocol`: 4 passed (RFC-8259 JSON, full 9-req/5-resp NDJSON round-trip, Phase 0 `pong`, 64 KiB & newline limits, surrogate escapes, `extract_seq_best_effort`)
    - `anime-lm-ngram`: 5 passed (`ANIMENGR` v1 roundtrip & corruption checks, OOV floor & unseen-bigram cap, Top-K 2nd-order Beam Viterbi disambiguation, SQLite WAL `user.db` persistence & privacy isolation)
    - `anime-engine`: 8 unit tests + 1 real 7-category L1+L2 integration test passed
    - `animed`: 3 passed (`--stdio` all-command dispatch + malformed/oversized recovery + empty-line tolerance, Unix socket `0700`/`0600` permissions + closed-loop `user.db` & L1 Layer 3 learning, async L3 `revision=1` push & stale-`seq` drop)
  - `cargo test --release -p anime-engine --test real_l1_l2_eval -- --nocapture`: loaded 120k L1 (205 ms) + OpenSLR 11 1M-line L2 (`vocab=65,535`, `2g=537,173`, `3g=368,160`, 16 MB in 31 ms);
    - `eval/benchmarks/core_v1.tsv` (1,400 cases, 7 categories): Base L1+L2 `exact` 99.0%, `typo_simple` 92.5%, `typo_phonetic` Top-3 85.5%, `context_realword` Top-1 65.0%/Top-3 91.5%, `sentence` Top-1 74.5%/Top-3 88.5%, keystroke latency `p50=291 us`/`p99=966 us`; Layered L1+L2 `exact` **100.0%**, `prefix` Top-5 **100.0%**, `context_realword` Top-3 **91.5%**, `sentence` Top-1 **75.5%**/Top-3 **89.5%**, latency `p50=297 us`/`p99=973 us`
    - `data/compiled/eval_large_open.tsv` (9,888 cases, 7 categories): `exact` 99.8%, `typo_simple` 93.2%, `typo_phonetic` Top-3 83.6%, `context_realword` Top-1 64.4%/Top-3 94.4%, `sentence` Top-1 60.6%/Top-3 77.4%, overall latency `p50=309 us`/`p99=1009 us` (`<= 5000 us` Phase 1 gate)
  - `cargo test --workspace`: all 38 workspace tests across 7 crates passed (0 warnings)
  - `cargo run -p anime-eval -- eval/fixtures/smoke.tsv`: `smoke.tsv` baseline passed
  - `cargo run -p animed -- --version`: `animed 0.1.0 (protocol v1)`
  - `printf '{"type":"ping"}\n{"type":"shutdown"}\n' | cargo run -p animed -- --stdio`: `{"type":"pong","version":"0.1.0"}`
  - `./scripts/verify-boundaries.sh .agents/tasks/task-03-ngram-and-engine.md`: `BOUNDARY OK (task-03-ngram-and-engine): 已检查 16 个文件，零越界`
