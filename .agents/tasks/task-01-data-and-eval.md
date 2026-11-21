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
- Status: [PENDING]
- Changed Files:
- Verification Output:
