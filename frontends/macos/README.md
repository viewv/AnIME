# AnIME macOS Frontend (Planned — Phase 3)

本目录为未来 **macOS IMKit** 前端保留（当前 Phase 0–2 优先聚焦 Linux Wayland + Fcitx5）。

## 规划集成方式

- 核心推理层（`crates/protocol`、`crates/lexicon`、`crates/lm-ngram`、`crates/lm-neural`、`crates/engine`）保持 **100% 平台无关**，不引入任何 Linux/Fcitx5 依赖。
- 后续可通过以下两种方式之一在 macOS `IMKInputController`（Swift / Objective-C++）中复用同一引擎：
  1. **同进程 C-ABI FFI**（新增薄 C 导出层编译 `libanime_core.a`）；
  2. **本地 Unix Socket NDJSON**（`~/Library/Caches/AnIME/engined.sock`，完全复用 `.agents/contracts/ipc-protocol.md`）。
- 运行时用户目录遵循 `.agents/contracts/data-format.md`（`~/Library/Application Support/AnIME/`）。
