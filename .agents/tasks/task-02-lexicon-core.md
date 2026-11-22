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
- Status: [DONE]
- Changed Files:
  - `crates/lexicon/src/lib.rs`
  - `crates/lexicon/src/format.rs`
  - `crates/lexicon/src/keyboard.rs`
  - `crates/lexicon/src/layered.rs`
  - `crates/lexicon/src/phonetic.rs`
  - `crates/lexicon/src/trie.rs`
  - `crates/lexicon/src/contractions.rs`
  - `crates/lexicon/src/lexicon.rs`
  - `crates/lexicon/tests/real_lexicon_eval.rs`
  - `crates/lexicon/README.md`
  - `.agents/tasks/task-02-lexicon-core.md`
- Verification Output:
  - `cargo check --workspace`: 0 errors, 0 warnings
  - `cargo test -p anime-lexicon` (debug): 8 lib unit tests + 2 real-lexicon integration tests passed (0 warnings)
  - `cargo test --release -p anime-lexicon -- --nocapture`: loaded 120,000 words & 54,073 typo keys from `data/compiled/lexicon.animelex.bin` in 185 ms;
    - `Lexicon` Base on `eval/benchmarks/core_v1.tsv` (1,000 word queries): `exact` Top1=99.0% (p50=11us, p99=27us), `prefix` Top1=68.0%/Top3=96.0%/Top5=99.5% (p50=111us, p99=166us), `typo_simple` Top1=92.5%/Top3=100.0%, `typo_prefix` Top1=39.0%/Top3=69.0%/Top5=79.5%, `typo_phonetic` Top1=69.5%/Top3=85.5%/Top5=91.5%; overall L1 latency `p50=386 us`, `p99=733 us` (< 1 ms)
    - `LayeredLexicon` (3-layer: Base 120k + external TSV extension + runtime learned `Antigravity`/`Hyprland`) on `eval/benchmarks/core_v1.tsv` (1,000 word queries): `exact` Top1=**100.0%** (p50=13us, p99=29us), `prefix` Top1=68.5%/Top3=96.5%/Top5=100.0% (p50=122us, p99=171us), `typo_simple` Top1=92.5%/Top3=100.0%, `typo_prefix` Top1=39.5%/Top3=69.5%/Top5=80.0%, `typo_phonetic` Top1=69.5%/Top3=85.5%/Top5=91.5%; overall 3-layer L1 latency `p50=394 us`, `p99=710 us` (< 1 ms)
    - `Lexicon` Base on `data/compiled/eval_large_open.tsv` (7,500 held-out word queries): `exact` Top1=99.8% (p50=11us, p99=25us), `prefix` Top3=95.6%/Top5=99.2%, `typo_simple` Top1=93.2%/Top3=98.4%, `typo_prefix` Top3=65.1%/Top5=73.8%, `typo_phonetic` Top3=83.6%; overall L1 latency `p50=354 us`, `p99=506 us` (< 1 ms)
  - `cargo test --workspace`: all 28 tests across 7 workspace crates passed (0 warnings)
  - `make eval`: `smoke.tsv` baseline passed
  - `./scripts/verify-boundaries.sh .agents/tasks/task-02-lexicon-core.md`: `BOUNDARY OK`
