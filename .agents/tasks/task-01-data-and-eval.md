---
task_id: "task-01-data-and-eval"
worktree: "../wt-anime-data-eval"
allowed_paths:
  - "scripts/data_pipeline/**"
  - "data/MANIFEST.yaml"
  - "crates/eval/**"
  - "eval/**"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
  - "crates/lm-neural/**"
  - "crates/engine/**"
  - "crates/animed/**"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/data-format.md"
  - ".agents/contracts/eval-schema.md"
  - ".agents/contracts/core-api.md"
acceptance_cmd: "cargo test -p anime-eval && cargo run -p anime-eval -- eval/fixtures/smoke.tsv && ./scripts/verify-boundaries.sh .agents/tasks/task-01-data-and-eval.md"
---

## Objective (目标)
构建 100% 开源可复现的英文词库/错拼/词频构建脚本并登记至 `data/MANIFEST.yaml`，同时将 `eval/` 评测集扩充为覆盖全 7 类场景的大规模离线基准，并在 `crates/eval` 中实现大小写敏感/忽略统计、门槛检查（`--gate`）与 JSON/Markdown 报告导出。

## Constraints (局部硬约束)
- 严禁引入任何闭源/商业输入法数据；所有数据源必须在 `data/MANIFEST.yaml` 写明开源协议、版本、URL、SHA256 与构建脚本。
- 严格遵循 `.agents/contracts/eval-schema.md` 的 TSV 5 列格式与 7 个标准场景枚举，确保 `typo_phonetic` 与 `sentence` 有充分样本。
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，立即暂停并在 `Blocked` 栏写明原因与申请路径。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [DONE]
- Changed Files:
  - `data/MANIFEST.yaml`
  - `scripts/data_pipeline/common.py`
  - `scripts/data_pipeline/fetch_sources.py`
  - `scripts/data_pipeline/build_misspellings.py`
  - `scripts/data_pipeline/build_lexicon.py`
  - `scripts/data_pipeline/build_ngram.py`
  - `scripts/data_pipeline/build_eval_benchmark.py`
  - `scripts/data_pipeline/test_pipeline.py`
  - `scripts/data_pipeline/README.md`
  - `eval/benchmarks/core_v1.tsv`
  - `eval/README.md`
  - `crates/eval/src/cli.rs`
  - `crates/eval/src/gate.rs`
  - `crates/eval/src/main.rs`
  - `crates/eval/src/metrics.rs`
  - `crates/eval/src/report.rs`
  - `crates/eval/src/tsv.rs`
  - `.agents/tasks/task-01-data-and-eval.md`
- Verification Output:
  - `cargo check --workspace`: 0 errors, 0 warnings
  - `cargo test --workspace`: 19 passed across 7 workspace crates (incl. `anime-eval` 7 tests validating 1,400-case `core_v1.tsv` 7-category uniqueness & semantic purity)
  - `cargo run --release -p anime-eval -- eval/fixtures/smoke.tsv --gate phase0`: TOTAL 14, Top1=28.6%, strict1=28.6%, p50=2us, p99=32us, Gate `phase0` PASS
  - `cargo run --release -p anime-eval -- eval/benchmarks/core_v1.tsv`: TOTAL 1400 (200 per category), `exact` Top1=99.5%, overall Top1=15.0%, strict1=14.7%, p50=2us, p99=72us
  - `cargo run --release -p anime-eval -- data/compiled/eval_large_open.tsv --json-out ...`: TOTAL 9888 (`exact`=1500, `prefix`=1500, `typo_simple`=1500, `typo_prefix`=1500, `typo_phonetic`=1500, `context_realword`=888, `sentence`=1500), `exact` Top1=99.6%, overall Top1=15.2%, p50=2us, p99=77us, JSON validated
  - `build_lexicon.py` + `build_misspellings.py --split train` on fetched SCOWL + `wordfreq` + Wikipedia + `crate-ci/typos`: 120,000 words, 57,117 train-split misspellings (`ANIMELEX` v1 built cleanly in `.gitignore`d `data/compiled/`)
  - `python3 -m unittest discover -s scripts/data_pipeline -p 'test_*.py' -v`: 12 passed, 0 failed
  - `python3 scripts/data_pipeline/fetch_sources.py --verify-only`: `MANIFEST OK: 7 个数据源全部通过开源协议与格式校验`
  - `make verify-infra`: `INFRA OK`
  - `./scripts/verify-boundaries.sh .agents/tasks/task-01-data-and-eval.md`: `BOUNDARY OK (task-01-data-and-eval): 已检查 9 个文件，零越界`
