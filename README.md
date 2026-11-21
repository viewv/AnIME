# AnIME

**AnIME**（An Intelligent English Input Method Engine）是一个**100% 开源、本地优先、拼音式交互**的英文输入法引擎。

> 目标：在 Linux（Wayland + Fcitx5 优先，后续扩展 macOS IMKit）上提供接近现代移动端智能键盘 + 桌面拼音输入法的英文打字体验——输入留在预编辑区，由引擎实时完成**前缀补全、物理邻键纠错、重度读音纠错、词格整句解码与神经语义重排/续写**，通过数字键 `1`–`9` 选词上屏。

## 核心特性设计

- **拼音式预编辑交互**：
  - 支持在预编辑区中保留空格进行**词组/整句组合**（`i wnt to eat an appie` → `1. I want to eat an apple`），也支持单词逐个按 `1` 选词上屏；
  - `1`–`9` 选择候选并支持智能尾空格 / 标点自动吞空格（`receive ` + `.` → `receive.`）；
  - `Enter` 随时原样上屏当前预编辑字母（方便代码、变量名、缩写）；`Shift` 快速切换直通模式。
- **三层低延迟混合架构（Rust 跨平台核心）**：
  - **L1 词级同步层（< 1 ms）**：紧凑词典 Trie + **QWERTY 物理邻键加权束搜索**（`appie → apple`、`beutif → beautiful`）+ **Double Metaphone 读音倒排纠错**（`numonia → pneumonia`）+ 人类常见错拼表与大小写/缩写恢复（`im → I'm`）。
  - **L2 统计整句层（< 2 ms）**：词格（Word Lattice）Viterbi + 2/3-gram 语言模型 + SQLite 本机个人词频学习（隐私框自动禁学）。
  - **L3 神经精排与续写层（异步 ≤ 30 ms，不阻塞按键）**：端侧小 Transformer 对真词误拼（`go form here → go from here`、`their/there`）与多词整句重排，并输出下一词/短语预测。
- **薄前端、强边界、可移植**：
  - 核心引擎（`crates/*`）不绑定任何 Linux/Fcitx5 API，通过跨平台 NDJSON IPC（Unix Socket / `--stdio`）与平台前端解耦；
  - 优先交付 `frontends/fcitx5/`（Linux Wayland C++20 插件），预留 `frontends/macos/`（IMKit）。

## 仓库结构

- `crates/protocol` — 领域类型与 NDJSON 契约模型
- `crates/lexicon` — L1 词典、模糊前缀/邻键纠错、Double Metaphone 读音纠错
- `crates/lm-ngram` — L2 词格 Viterbi 解码、2/3-gram 与 SQLite 用户学习
- `crates/lm-neural` — L3 小 Transformer 异步重排与续写预测接口及实现
- `crates/engine` — 平台无关会话状态机（预编辑/部分选词/融合排序/智能空格）
- `crates/animed` — 独立守护进程（Unix Socket + `--stdio`）
- `crates/eval` & `eval/` — 7 类离线评测集与 Top-K / 延迟基准工具
- `frontends/fcitx5` — Fcitx5 C++20 薄前端插件
- `scripts/data_pipeline` & `data/MANIFEST.yaml` — 可复现开源词库/语料清单与构建脚本
- `AGENTS.md` & `.agents/` — Agent 模块边界地图、锁定契约与任务单流水线

## 快速构建与检查

```bash
make check          # 检查 Rust workspace（0 警告）
make test           # 运行全量单元测试
make engine         # 构建 release 版 animed
make addon          # 编译 Fcitx5 插件 frontends/fcitx5/build/libanime.so
make eval           # 运行 eval/fixtures/smoke.tsv 烟雾评测
make verify-infra   # 校验 Agent 基础设施与边界守卫
```

## 开源协议

本项目代码采用 [Mozilla Public License 2.0 (LICENSE)](LICENSE)。所有词库、错拼数据集、语料与模型权重均须严格遵循各自开源协议并在 [data/MANIFEST.yaml](data/MANIFEST.yaml) 登记来源。
