# AnIME 开发与 Agent 协作规范

AnIME（An Intelligent English Input Method Engine）是一个**100% 开源洁净、平台无关核心**的拼音式英文输入法：支持带空格的句子级预编辑、QWERTY 物理邻键加权纠错、重度读音式拼写纠错、2/3-gram 词格整句解码与本机学习，以及后续的端侧小 Transformer 异步精排与续写预测。现阶段优先适配 **Linux Wayland + Fcitx5**，核心层保持纯平台无关以便后续扩展 **macOS IMKit**。

> **唯一权威入口**：本文件 + `.agents/map.yaml` + `.agents/contracts/*.md`。所有跨模块语义、IPC 线协议、二进制格式与评测规范均以 `.agents/contracts/` 为唯一准绳。

---

## 1. Cold-Start Protocol（启动三步走，严禁全仓乱扫）

1. **读任务单或查地图**：
   - 若被分配了 `.agents/tasks/<task-id>.md`，第一时间完整读取该任务单，明确 `allowed_paths`、`forbidden_paths`、`read_only_context` 和 `acceptance_cmd`。
   - 若为无任务单的调研/规划请求，先读取 `.agents/map.yaml` 定位模块归属与依赖关系。
2. **锁定工作边界**：
   - 对照 `.agents/map.yaml` 确认目标模块的路径范围、构建命令、测试命令与依赖模块，只打开职责范围内的源码文件。
3. **只读指定契约**：
   - 仅读取任务单 `read_only_context` 列出的 `.agents/contracts/*.md` 或目标库对外暴露的 Trait / 公共接口定义。**禁止**在未授权时全仓扫描无关 crate、`data/raw/` 或第三方语料目录。

---

## 2. 架构速览

```text
Thin Frontends（平台相关层，只做按键捕获 / UI / surrounding text / IPC）
  ├── frontends/fcitx5/   (C++20 Fcitx5 插件 — Phase 1 优先目标，Wayland/KDE/GNOME)
  └── frontends/macos/    (未来 macOS IMKit 前端，IMKInputController)
          │
          ▼  NDJSON over Unix Socket (或未来 C-ABI FFI)
          │  Linux: $XDG_RUNTIME_DIR/anime/engined.sock | CLI/测试: animed --stdio
          │
Daemon & Engine Core（Rust Workspace，核心 crates 零平台依赖）
  ├── crates/animed       守护进程入口（Unix Socket + --stdio，调度异步 L3）
  ├── crates/engine       核心会话状态机（预编辑/部分选词/智能空格/多路融合排序）
  ├── crates/lexicon      L1 词级层（紧凑 Trie + QWERTY 邻键纠错 + Double Metaphone + 缩写大小写恢复）
  ├── crates/lm-ngram     L2 统计层（词格 Viterbi 解码 + 2/3-gram + SQLite 个人学习）
  ├── crates/lm-neural    L3 神经层（异步小 Transformer 重排与短语/整句续写预测）
  └── crates/protocol     平台无关领域类型与 NDJSON 契约模型

Offline Data & Benchmarking
  ├── scripts/data_pipeline/  可复现开源词库与语料构建脚本（登记于 data/MANIFEST.yaml）
  └── crates/eval + eval/     7 类场景自动化评测集与 Top-K / 延迟门禁统计
```

---

## 3. Build & Verification Commands

```bash
# Workspace 检查与单元测试
make check              # cargo check --workspace
make test               # cargo test --workspace
make engine             # cargo build --release -p animed

# Fcitx5 C++20 前端编译（输出 frontends/fcitx5/build/libanime.so）
make addon

# 离线评测与协议烟雾测试
make eval               # cargo run -p anime-eval -- eval/fixtures/smoke.tsv
cargo run -p animed -- --version
printf '{"type":"ping"}\n{"type":"shutdown"}\n' | cargo run -p animed -- --stdio

# Agent 工程基建自检与任务单边界守卫
make verify-infra
./scripts/verify-boundaries.sh .agents/tasks/<task-id>.md
./scripts/verify-boundaries.sh .agents/tasks/<task-id>.md --base main --tracked-only

# 并行 Worktree 管理（共享主仓 target/ 以避免重复编译）
./scripts/agent-wt.sh create <task-id>
./scripts/agent-wt.sh list
./scripts/agent-wt.sh remove <task-id>
```

---

## 4. Hard Rules（不可逾越红线）

1. **禁止跨界修改**：有任务单时，绝不修改 `allowed_paths` 之外或命中 `forbidden_paths` 的任何文件；**必须跨界或改动公共契约时，必须立即暂停**，在任务单 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限，获批准并更新任务单白名单后方可继续。
2. **Worktree 与 Git 纪律**：任务 Agent 允许在独立 worktree 分支本地提交（中文提交说明 + `Co-Authored-By` 行），但**合并进 `main` 与 `git push` 必须由用户明确确认**；禁止使用 `git add -A`；并行开发必须使用 `./scripts/agent-wt.sh` 并共享 `CARGO_TARGET_DIR`。
3. **100% 开源洁净数据红线**：严禁引入任何商业/闭源输入法二进制、词库、语言模型或未验证来源的权重；所有词库、错拼表、语料与模型必须登记在 `data/MANIFEST.yaml`（记录来源 URL、版本、开源协议白名单、SHA256 与可复现构建脚本）。
4. **核心跨平台红线**：`crates/protocol`、`crates/lexicon`、`crates/lm-ngram`、`crates/lm-neural`、`crates/engine` 严禁引入 Linux/Fcitx5/Wayland/X11 专有依赖或系统调用，确保未来 macOS IMKit 可直接复用同一核心。
5. **系统安全红线**：禁止执行 `sudo`（需要提权安装的命令只输出给用户自行执行）；禁止在 KDE Wayland 下手动启动 `fcitx5`；绝不跨目录删除或覆写 `/usr/lib/fcitx5` 与 `/usr/lib64/fcitx5`。
6. **量化验证即交付**：改动 `lexicon`、`lm-ngram`、`lm-neural` 或 `engine` 时，任务结束前必须亲自跑通 `make test`、`make eval` 与 `./scripts/verify-boundaries.sh`，并将实际测试摘要与评测指标写入任务单 `Handoff / Evidence`。
7. **注释规范**：禁止写「为了修复 XX 而添加」的流水账注释；必须保留并写清解释键盘物理距离模型、Double Metaphone 读音规则、IPC 时序不变量（`seq`/`revision`）与延迟预算的 Why 注释。
