# AnIME 离线评测集与门禁说明

本目录依据 [.agents/contracts/eval-schema.md](../.agents/contracts/eval-schema.md) 维护英文输入法引擎的 7 类场景离线基准测试集。

## 数据集清单

| 文件 | 样本数 | 用途 |
|---|---:|---|
| [fixtures/smoke.tsv](fixtures/smoke.tsv) | 14 | 快速烟雾评测（每类 2 条，`make eval` 默认入口） |
| [benchmarks/core_v1.tsv](benchmarks/core_v1.tsv) | 396 | 核心离线基准评测（全面覆盖 7 类场景，重点强化 `typo_phonetic` 与 `sentence`） |

## 7 类评测场景

1. `exact`：常规拼对英文词、技术新词、缩写与专有名词，验证零误纠伤。
2. `prefix`：标准前缀补全。
3. `typo_simple`：单键 QWERTY 邻键误触、漏打、多打、相邻换位。
4. `typo_prefix`：前部拼错 + 尚未打完的截断前缀（如 `beutif → beautiful`）。
5. `typo_phonetic`：重度读音相似拼错 / 哑字母 / 多重编辑距离（如 `numonia → pneumonia`）。
6. `context_realword`：依赖上文的真词误拼消歧（`from/form`、`their/there/they're` 等）及句首大写。
7. `sentence`：预编辑区带空格多词整句与复合错误恢复。

## 运行命令示例

```bash
# 运行默认烟雾测试
make eval

# 运行 core_v1 全量评测并导出 JSON 与 Markdown 报告
cargo run -p anime-eval -- eval/benchmarks/core_v1.tsv \
  --json-out /tmp/core_v1.json \
  --md-out /tmp/core_v1.md

# 启用阶段门禁检查（phase0 / phase1 / phase2 或自定义规则，不达标返回退出码 2）
cargo run -p anime-eval -- eval/fixtures/smoke.tsv --gate phase0
cargo run -p anime-eval -- eval/benchmarks/core_v1.tsv --gate phase1 --show-failures 15
cargo run -p anime-eval -- eval/benchmarks/core_v1.tsv --gate exact.strict_top1>=98 --gate sentence.top3>=80
```
