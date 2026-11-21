# CLAUDE.md

本仓库的所有 AI Agent（含 Claude / Antigravity / 独立子任务 Agent）**必须严格遵循 [AGENTS.md](AGENTS.md) 作为唯一权威入口**：

1. **冷启动先查**：读分配的任务单 `.agents/tasks/<task-id>.md` -> 对照 `.agents/map.yaml` -> 只读 `read_only_context` 指定的 `.agents/contracts/*.md`。
2. **禁止越界**：严格遵守 `allowed_paths` 与 `forbidden_paths`；如需跨模块或修改契约，立即在任务单 `Blocked` 栏写明原因与目标路径并暂停等待批准。
3. **交付门禁**：提交或交付前必须通过 `make test` / 对应构建命令与 `./scripts/verify-boundaries.sh .agents/tasks/<task-id>.md`，并在 `Handoff / Evidence` 如实记录终端输出。
