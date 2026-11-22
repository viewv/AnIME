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
| `expected` | 期望第 1 或 Top-K 命中的规范输出文本；可用 ` \|\| ` 分隔多个**同等可接受**的参考答案 | 忽略首尾空格进行比较（精确匹配小写或期望大小写，报告同时可输出大小写严格率）；第一个参考为展示用主答案 |
| `notes` | 人类可读备注，可选以标签开头 | 说明测什么错误模式，不参与匹配；标签约定见下 |

- **多参考答案**：`expected` 按 `||` 切分后逐个去首尾空格，任一参考为空即为格式错误。Top-K 命中定义为**任一参考**位于候选前 K 位；strict 指标同理按大小写精确比较。仅一个参考的行与旧格式完全兼容。多参考只用于真正同等合理的输出（如截断前缀 `grenad` 的 `grenade || grenades`），不得用来掩盖错误。
- **`notes` 标签**：若 `notes` 以 `<namespace>:<tag>` 开头（首个空白前的 token，`[a-z_]+:[a-z_]+`），该 token 作为分组标签，其余为自由文本。保留命名空间：
  - `noise:<type>`：合成真实打字噪声，`<type>` 取 `leading_drop` / `boundary_gemination` / `space_shift` / `space_extra` / `space_missing` / `adjacent_key` / `realword_edit` / `contraction`；
  - `negative:clean`：干净输入 `input == expected`，用于监控误纠率；
  - `source:<name>`：样本来源（如 `source:multiwoz`），可选。

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
- `key_latency_p50_us`：**单次按键**同步 L1+L2 耗时（微秒）中位数，统计全部样本的每一次 `handle_key` 调用
- `key_latency_p99_us`：单次按键耗时 99 分位
- `key_latency_max_us`：单次按键耗时最大值（预编辑 `<= 80` 字符的样本）
- `sample_total_p50_us` / `sample_total_p99_us`（仅参考，不作门禁）：单样本从首键到末键的累计耗时，随输入长度线性增长，不能与单键预算直接比较

> 延迟门禁**只认单键指标**，与 `core-api.md` §5 的同步路径预算一一对应。L3 延迟按单次推理调用单独统计。

- **交互指标（`anime-eval --interactive`，可选，不作现有门禁）**：逐键回放时每键后检查期望答案排名（检查不计入单键延迟），按类别 / 标签 / TOTAL 输出：
  - `keys_saved_top1_pct` / `keys_saved_top3_pct`：期望答案首次进入 Top-1 / Top-3 时剩余未敲键占全长比例的均值（从未进入计 0）；
  - `top1_dropouts_per_sample`：期望答案进入 Top-1 后又被挤出的平均次数（候选跳动）；
  - `lost_after_top1_pct`：曾进入 Top-1 但最终不在 Top-1 的样本占比；
  - `mean_select_index`：最终候选中期望答案的平均 1 起序号，未命中计 10；
  - `top1_ci95`：Top-1 的 Wilson 95% 置信区间（百分比），用于判断差异是否超出抽样误差。
- **按标签分组**：`anime-eval --group-by-tag` 额外按 `notes` 标签（§1）输出每个标签的 `total` / `top1` / `top3` / `top5` / strict 指标；无标签样本归入 `untagged`。门禁可用 `tag:<namespace>:<tag>.<metric>` 作为目标（如 `tag:negative:clean.top1>=97`）。

### 阶段门禁参考线

| 里程碑 | `exact` Top1（防误纠） | `typo_simple` Top1 | `typo_phonetic` Top3 | 同步 L1+L2 延迟 |
|---|---|---|---|---|
| Phase 0（骨架烟雾） | 基线记录 | 基线记录 | 基线记录 | 单样本 < 500 us（历史口径） |
| Phase 1（L1 + L2 MVP） | >= 98% | >= 85% | >= 75% | 单样本 p99 <= 5 ms（历史口径） |
| Phase 2（+ L3 神经） | >= 99% | >= 92% | >= 85%（+ `context_realword` >= 85%） | 单样本 p99 <= 5 ms（历史口径），L3 <= 30 ms |
| Phase 3 起（现行） | >= 99% | >= 92% | >= 85%（+ `context_realword` >= 85%） | 单键 p50 <= 3 ms、p99 <= 10 ms、max <= 16 ms；L3 <= 30 ms |
