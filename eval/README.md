# AnIME 离线评测集与门禁说明

本目录依据 [.agents/contracts/eval-schema.md](../.agents/contracts/eval-schema.md) 维护英文输入法引擎的 7 类场景离线基准测试集，采用分层基准避免样本量不足或过拟合。

## 分层基准体系

| 层级 | 文件位置 | 样本规模 | 定位与数据源 |
|---|---|---:|---|
| Tier 1 (Smoke) | [fixtures/smoke.tsv](fixtures/smoke.tsv) | 14 | 毫秒级冒烟评测（每类 2 条，`make eval` 默认入口） |
| Tier 2 (Golden Core) | [benchmarks/core_v1.tsv](benchmarks/core_v1.tsv) | **1,400** | 入库核心基准（每类 200 条：`exact` 0.5% 分辨率；融合原创精校样本、SCOWL + `wordfreq`、`crate-ci/typos` held-out 切分与 MultiWOZ 2.2 真实对话） |
| Tier 3 (Large Open) | `data/compiled/eval_large_open.tsv`（本地构建，已忽略） | **~9,888** | 万级全量真实评测集，由 [build_eval_benchmark.py](../scripts/data_pipeline/build_eval_benchmark.py) 基于 [MANIFEST.yaml](../data/MANIFEST.yaml) 开源数据生成（含防泄漏 `--split eval` 切分） |
| 可选本机扩展 | `data/compiled/eval_local_*.tsv`（本地生成，已忽略） | 按需 | 通过 `--local-birkbeck` / `--local-jfleg-src` 转换用户本机非商用研究数据集，仅用于本地补充评测，严禁混入训练或提交进 Git |

## 7 类评测场景

1. `exact`：常规拼对英文词、技术新词、缩写与专有名词，验证零误纠伤。
2. `prefix`：标准无歧义前缀补全。
3. `typo_simple`：单键 QWERTY 邻键误触、漏打、多打、相邻换位（含 `crate-ci/typos` 真实开发错拼）。
4. `typo_prefix`：前部拼错 + 尚未打完的截断前缀（如 `beutif → beautiful`）。
5. `typo_phonetic`：重度读音相似拼错 / 哑字母 / 多重编辑距离（如 `numonia → pneumonia`）。
6. `context_realword`：依赖上文的真词误拼消歧（`from/form`、`their/there/they're`、`your/you're` 等来自 MultiWOZ 2.2 真实对话语境）及句首大写。
7. `sentence`：预编辑区带空格多词整句与复合错误恢复（含 MultiWOZ 2.2 自然对话句）。

## 运行命令示例

```bash
# 运行默认烟雾测试
make eval

# 运行 1,400 条核心基准并导出 JSON 与 Markdown 报告
cargo run --release -p anime-eval -- eval/benchmarks/core_v1.tsv \
  --json-out /tmp/core_v1.json \
  --md-out /tmp/core_v1.md

# 运行本地生成的 9,888 条大规模真实评测集
cargo run --release -p anime-eval -- data/compiled/eval_large_open.tsv

# 启用阶段门禁检查（phase0 ~ phase3 或自定义规则；延迟指标为单键口径 key_p50_us / key_p99_us / key_max_us，不达标返回退出码 2）
cargo run -p anime-eval -- eval/fixtures/smoke.tsv --gate phase0
cargo run --release -p anime-eval -- eval/benchmarks/core_v1.tsv --gate phase1 --show-failures 15
cargo run --release -p anime-eval -- eval/benchmarks/core_v1.tsv --gate phase3
cargo run --release -p anime-eval -- eval/benchmarks/core_v1.tsv --gate exact.strict_top1>=98 --gate sentence.top3>=80
```
