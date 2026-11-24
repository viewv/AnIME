# anime-eval：分模式回放口径

`anime-eval` 逐键回放 `eval/` 下的 TSV（格式见 `.agents/contracts/eval-schema.md`）。引擎有两种空格语义，
各自的打分参数存放在 `crates/engine/src/profile.rs` 的 `ModeProfile` 中（`WORD_PROFILE` / `SENTENCE_PROFILE`），
由 `EngineConfig.space_commits` 选择。`--mode` 只设置这一项，回放器按该模式的空格语义驱动输入：

| `--mode` | `space_commits` | 空格键 | 参与排名的文本 |
|---|---|---|---|
| `sentence`（默认） | `false` | 追加到预编辑区，整串由 L2 词格解码 | 最终预编辑的候选（与旧报告完全相同） |
| `word` | `true` | 上屏首选（标点同理） | 本样本已上屏文本 + 剩余预编辑的每个候选；预编辑为空时只有已上屏文本 |

单词模式下无空格连打（`iwanttogo`）仍在单 token 内切分并走词格，所以它同样用到 `WORD_PROFILE` 的
`runon.*` / `lattice.*` 参数。L3（`--model`）只在当前模式的 `l3.enabled` / `l3.min_tokens` 允许时调用，
与 animed 的调度一致；单词模式下 L3 上文取引擎已推进的上文（含本样本已上屏的词）。

## 各评测集怎么跑

| 评测集 | 主口径 | 说明 |
|---|---|---|
| `core_v2` 的 `exact` / `prefix` / `typo_simple` / `typo_prefix` / `typo_phonetic` | 两种模式结果相同 | 输入没有空格，两种模式逐行一致。它们是**单词模式排序**的主要指标（单词模式受排序限制：top1 与 top5 之差最大）。 |
| `core_v2` 的 `context_realword` | `--mode word` 与 `--mode sentence` 都看 | 多数行是"上文 + 两三个词"（`form here`）。单词模式下前面的词按首选逐个上屏，更接近 `SpaceCommits=True` 用户的真实体验。 |
| `core_v2` 的 `sentence`、`noise_v2`、`ood_dev_v1`、`ood_academic_v1`、`ood_tm2_v1` | `--mode sentence` | 整句召回的主指标（整句模式受召回限制）。`--mode word` 的结果表示"同一句话按单词模式逐词敲"，但前面的词已被首选锁定，top3 / top5 基本等于 top1，**不能与整句模式的 top-K 直接比较**。 |
| `ood_v1`（文学） | 仅参考 | 不作门禁。 |

单词模式调参轨道只改 `WORD_PROFILE`，并以 `--mode word` 的结果为准；整句召回轨道只改 `SENTENCE_PROFILE`，
以 `--mode sentence` 的结果为准。两条轨道都要确认另一模式逐行不变（`--dump-rows` 输出 diff 为空）。

## 命令

```bash
# 两种模式各跑全部冻结集，报告和逐行结果写到 target/eval-modes/
make eval-modes
make eval-word EVAL_SETS=core_v2
make eval-sentence EVAL_LEX=/path/to/lexicon.animelex.bin EVAL_NGRAM=/path/to/ngram.animengr.bin

# 不重编译地覆盖 profile 字段（MODE = word | sentence | both，可重复）
cargo run --release -p anime-eval -- eval/benchmarks/core_v2.tsv \
  --lexicon data/compiled/lexicon.animelex.bin --ngram ~/.local/share/anime/ngram.animengr.bin \
  --mode word --profile word.confusables.adjacent_key_single_token_min_penalty=3.5 \
  --group-by-tag --dump-rows /tmp/core_v2.word.rows.tsv
make eval-word EVAL_PROFILE="--profile word.l2.context_weight=1.2"

# 列出全部可覆盖字段及其基准值
cargo run --release -p anime-eval -- --profile list
```

`--dump-rows` 每行一条：`category  context  input  expected  ci_rank  top1  top5`（`top5` 用 ` | ` 分隔），
同一 TSV 的两次运行可直接 `diff` 或 `cmp`，用于证明某一模式逐行不变。
