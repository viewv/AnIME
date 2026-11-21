---
task_id: "<task-id>"
worktree: "../wt-<short-name>"
allowed_paths:
  - "<module-or-file-glob>"
forbidden_paths:
  - ".agents/contracts/**"
read_only_context:
  - ".agents/contracts/<relevant-contract>.md"
acceptance_cmd: "<build-and-test-command> && ./scripts/verify-boundaries.sh .agents/tasks/<task-id>.md"
---

## Objective (目标)
一句话写清本任务要实现的目标与预期效果。

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
