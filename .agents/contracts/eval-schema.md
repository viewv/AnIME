# Contract: Offline Evaluation Format & Gate Rules (`eval-schema.md`)

> 状态：**LOCKED**。所有评测集文件（`eval/**/*.tsv`）与 `crates/eval` 报告格式须遵循本契约。

---

## 1. TSV 评测文件格式

- UTF-8 文本，无 BOM；空行与 `#` 开头注释行忽略。
- 每条样本 **5 列**，以单制表符 `<TAB>` 分隔：

```text
category<TAB>context<TAB>input<TAB>expected<TAB>notes
```

| 列名 | 含义 | 说明 |
|---|---|---|
| `category` | 场景类别（见 §2 枚举） | 严格小写下划线命名 |
| `context` | 光标前文字符串 | 无上文时留空（空字符串，保留 Tab 分隔符） |
| `input` | 模拟用户在预编辑区输入的完整字母/空格串 | 评测时按当前引擎会话逐字符喂入 |
| `expected` | 期望第 1 或 Top-K 命中的规范输出文本 | 忽略末尾自动空格进行比较（精确匹配小写或期望大小写，报告同时可输出大小写严格率） |
| `notes` | 人类可读备注 | 说明测什么错误模式，不参与匹配 |

---

## 2. 7 类必备评测场景（`category`）

1. `exact`：常规拼对英文单词（如 `apple` → `apple`、`kubernetes` → `kubernetes`），用于校验**零误纠伤**。
2. `prefix`：标准前缀补全（如 `beautif` → `beautiful`、`algor` → `algorithm`）。
3. `typo_simple`：单键邻键误触/漏打/多打/相邻换位（如 `appie` → `apple`、`teh` → `the`、`recieve` → `receive`、`langauge` → `language`）。
4. `typo_prefix`：**前部拼错 + 尚未打完的截断前缀**（如 `beutif` → `beautiful`、`enviorn` → `environment`）。
5. `typo_phonetic`：**重度读音相似拼错 / 多重编辑距离**（如 `numonia` → `pneumonia`、`definately` → `definitely`、`restrant` → `restaurant`、`fisics` → `physics`）。
6. `context_realword`：依赖上文的**真词误拼与词形消歧**（如上文 `go ` 输入 `form here` → `from here`；上文 `their ` / `there ` 语境选词）。
7. `sentence`：预编辑区带空格多词整句与复合错误恢复（如 `i wnt to eat an appie` → `I want to eat an apple`）。

---

## 3. 必备指标与输出规范

`anime-eval` 对全量与分场景分别统计并打印以下指标：
- `total`：样本数
- `top1`：期望结果位于候选第 1 位的命中率（`hits / total`）
- `top3`：位于前 3 位命中率
- `top5`：位于前 5 位命中率
- `latency_p50_us`：单样本查询总耗时（微秒）中位数
- `latency_p99_us`：单样本查询总耗时（微秒）99 分位

### 阶段门禁参考线（供 Phase 1 / Phase 2 验收对比）

| 里程碑 | `exact` Top1（防误纠） | `typo_simple` Top1 | `typo_phonetic` Top3 | 同步 L1+L2 `p99` |
|---|---|---|---|---|
| Phase 0（骨架烟雾） | 基线记录 | 基线记录 | 基线记录 | < 500 us |
| Phase 1（L1 + L2 MVP） | >= 98% | >= 85% | >= 75% | <= 5000 us (5 ms) |
| Phase 2（+ L3 神经） | >= 99% | >= 92% | >= 85%（+ `context_realword` >= 85%） | L1+L2 <= 5 ms，L3 <= 30 ms |
