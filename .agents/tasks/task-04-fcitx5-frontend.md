---
task_id: "task-04-fcitx5-frontend"
worktree: "../wt-anime-fcitx5"
allowed_paths:
  - "frontends/fcitx5/**"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/**"
  - "scripts/**"
  - "data/**"
  - "eval/**"
read_only_context:
  - ".agents/contracts/ipc-protocol.md"
  - ".agents/contracts/core-api.md"
acceptance_cmd: "cmake -S frontends/fcitx5 -B frontends/fcitx5/build && cmake --build frontends/fcitx5/build -j$(nproc) && ./scripts/verify-boundaries.sh .agents/tasks/task-04-fcitx5-frontend.md"
---

## Objective (目标)
在 `frontends/fcitx5/` 实现面向 Linux Wayland + Fcitx5 的模块化 C++20 薄前端插件：异步 Unix Socket NDJSON IPC 客户端（接入 `fcitx::EventLoop` IO 监听）、带空格拼音式预编辑展示、`1`–`9` 选词上屏与部分选词、`Enter` 原串上屏、单按 `Shift` 切换直通模式、光标上下文（surrounding text）采集、隐私框（Password/NoPersist）屏蔽以及 `seq`/`revision` 防过期更新。

## Constraints (局部硬约束)
- 保持薄前端定位，拆分网络通信、状态/候选列表与 Fcitx5 适配器，严禁在插件内写词典/纠错/语言模型逻辑。
- 编译要求 `-Wall -Wextra` 零警告；不得直接执行 `sudo` 或手动启动 `fcitx5`。
- 严格遵循 `.agents/contracts/ipc-protocol.md` 消息结构与 `0600` Socket 连接逻辑。
- 仅改动 `allowed_paths` 内文件；必须跨界或改契约时，立即暂停并在 `Blocked` 栏申请。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
