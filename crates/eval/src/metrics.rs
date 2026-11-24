//! 评测执行器与统计指标：同时统计忽略大小写与大小写严格 Top-1/3/5 及单键 / 单样本微秒分位延迟。

use std::collections::BTreeMap;
use std::time::Instant;

use anime_engine::{EngineOutput, EngineSession};
use anime_lexicon::{LexiconProvider, StaticWordList};
use anime_lm_neural::{NeuralRuntimeInfo, NeuralSentenceModel, NoopNeuralModel};
use anime_lm_ngram::{NgramModel, NoopNgramModel};
use anime_protocol::{CandidateKind, KeyInput};

use crate::tsv::{Category, EvalRow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleFailure {
    pub category: Category,
    pub context: String,
    pub input: String,
    pub expected: String,
    pub top1_actual: Option<String>,
    pub ci_rank: Option<usize>,
    pub cs_rank: Option<usize>,
}

/// 单行最终结果，供 `--dump-rows` 逐行对比（中立性验证、调参前后 diff）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowOutcome {
    pub category: Category,
    pub context: String,
    pub input: String,
    pub expected: String,
    pub ci_rank: Option<usize>,
    /// 最终候选文本（单词模式下已拼接此前上屏的文本），最多保留前 5 条。
    pub top5: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CategoryStats {
    pub total: usize,
    pub top1: usize,
    pub top3: usize,
    pub top5: usize,
    pub strict_top1: usize,
    pub strict_top3: usize,
    pub strict_top5: usize,
    /// 每一次 `handle_key` 的同步 L1+L2 耗时，门禁口径（`eval-schema.md` §3）。
    pub key_latencies_us: Vec<u128>,
    /// 预编辑不超过 [`MAX_KEY_PREEDIT_CHARS`] 字符时的单键最大耗时。
    pub key_max_us: u128,
    /// 单样本首键到末键累计耗时，随输入长度线性增长，仅作参考不作门禁。
    pub sample_total_us: Vec<u128>,
    /// 打字过程交互指标累计（`eval-schema.md` §3 交互指标）。
    pub interaction: InteractionTotals,
}

/// 逐键回放得到的单样本交互轨迹。
#[derive(Debug, Default, Clone)]
pub struct SampleInteraction {
    /// 输入总键数。
    pub keys: usize,
    /// 期望答案首次位于 Top-1 / Top-3 时已敲键数（从 1 计）；从未进入为 `None`。
    pub first_top1_key: Option<usize>,
    pub first_top3_key: Option<usize>,
    /// 期望答案进入 Top-1 后又被挤出的次数（候选跳动）。
    pub top1_dropouts: usize,
    /// 最终候选中的位置（0 起），未命中为 `None`。
    pub final_rank: Option<usize>,
}

impl SampleInteraction {
    /// 记录一次按键后的期望答案排名，`key_idx` 从 1 计。
    pub fn observe(&mut self, key_idx: usize, rank: Option<usize>, was_top1: &mut bool) {
        let now_top1 = rank == Some(0);
        if now_top1 && self.first_top1_key.is_none() {
            self.first_top1_key = Some(key_idx);
        }
        if rank.is_some_and(|r| r < 3) && self.first_top3_key.is_none() {
            self.first_top3_key = Some(key_idx);
        }
        if *was_top1 && !now_top1 {
            self.top1_dropouts += 1;
        }
        *was_top1 = now_top1;
    }
}

/// 交互指标的类别级累计；`unranked_index` 为未命中时计入的选择序号（候选上限 + 1）。
#[derive(Debug, Default, Clone)]
pub struct InteractionTotals {
    pub samples: usize,
    pub saved_top1_sum: f64,
    pub saved_top3_sum: f64,
    pub dropouts_sum: usize,
    pub lost_after_top1: usize,
    pub select_index_sum: f64,
}

/// 未命中样本计入的选择序号：默认候选页 9 项之外（第 10 位）。
pub const UNRANKED_SELECT_INDEX: f64 = 10.0;

impl InteractionTotals {
    pub fn add(&mut self, s: &SampleInteraction) {
        if s.keys == 0 {
            return;
        }
        let saved = |first: Option<usize>| {
            first.map_or(0.0, |k| (s.keys - k) as f64 / s.keys as f64)
        };
        self.samples += 1;
        self.saved_top1_sum += saved(s.first_top1_key);
        self.saved_top3_sum += saved(s.first_top3_key);
        self.dropouts_sum += s.top1_dropouts;
        if s.first_top1_key.is_some() && s.final_rank != Some(0) {
            self.lost_after_top1 += 1;
        }
        self.select_index_sum += s.final_rank.map_or(UNRANKED_SELECT_INDEX, |r| (r + 1) as f64);
    }

    fn mean(&self, v: f64) -> f64 {
        if self.samples == 0 { 0.0 } else { v / self.samples as f64 }
    }

    /// 省键率：期望答案首次进入 Top-1 时剩余未敲键占比的均值（从未进入计 0）。
    pub fn keys_saved_top1_pct(&self) -> f64 {
        100.0 * self.mean(self.saved_top1_sum)
    }

    pub fn keys_saved_top3_pct(&self) -> f64 {
        100.0 * self.mean(self.saved_top3_sum)
    }

    /// 每样本期望答案从 Top-1 被挤出的平均次数。
    pub fn dropouts_per_sample(&self) -> f64 {
        self.mean(self.dropouts_sum as f64)
    }

    /// 曾进入 Top-1 但最终不在 Top-1 的样本占比。
    pub fn lost_after_top1_pct(&self) -> f64 {
        100.0 * self.mean(self.lost_after_top1 as f64)
    }

    pub fn mean_select_index(&self) -> f64 {
        self.mean(self.select_index_sum)
    }
}

/// 单键最大值门禁只覆盖该长度以内的预编辑，对应 `core-api.md` §5 的 `<= 80` 字符约定。
pub const MAX_KEY_PREEDIT_CHARS: usize = 80;

/// 一条样本逐键回放的计时结果。
#[derive(Debug, Default, Clone)]
pub struct SampleTiming {
    pub key_us: Vec<u128>,
    /// 预编辑 `<= MAX_KEY_PREEDIT_CHARS` 字符的按键中的最大耗时。
    pub key_max_us: u128,
}

impl SampleTiming {
    pub fn total_us(&self) -> u128 {
        self.key_us.iter().sum()
    }
}

impl CategoryStats {
    pub fn record(
        &mut self,
        ci_rank: Option<usize>,
        cs_rank: Option<usize>,
        timing: &SampleTiming,
    ) {
        self.total += 1;
        if let Some(idx) = ci_rank {
            if idx == 0 {
                self.top1 += 1;
            }
            if idx < 3 {
                self.top3 += 1;
            }
            if idx < 5 {
                self.top5 += 1;
            }
        }
        if let Some(idx) = cs_rank {
            if idx == 0 {
                self.strict_top1 += 1;
            }
            if idx < 3 {
                self.strict_top3 += 1;
            }
            if idx < 5 {
                self.strict_top5 += 1;
            }
        }
        self.key_latencies_us.extend_from_slice(&timing.key_us);
        self.key_max_us = self.key_max_us.max(timing.key_max_us);
        self.sample_total_us.push(timing.total_us());
    }

    pub fn finalize(&mut self) {
        self.key_latencies_us.sort_unstable();
        self.sample_total_us.sort_unstable();
    }

    pub fn pct(hits: usize, total: usize) -> f64 {
        if total == 0 {
            0.0
        } else {
            (hits as f64) * 100.0 / (total as f64)
        }
    }

    pub fn top1_pct(&self) -> f64 {
        Self::pct(self.top1, self.total)
    }

    pub fn top3_pct(&self) -> f64 {
        Self::pct(self.top3, self.total)
    }

    pub fn top5_pct(&self) -> f64 {
        Self::pct(self.top5, self.total)
    }

    pub fn strict_top1_pct(&self) -> f64 {
        Self::pct(self.strict_top1, self.total)
    }

    pub fn strict_top3_pct(&self) -> f64 {
        Self::pct(self.strict_top3, self.total)
    }

    pub fn strict_top5_pct(&self) -> f64 {
        Self::pct(self.strict_top5, self.total)
    }

    fn sorted_percentile(values: &[u128], p: usize) -> u128 {
        if values.is_empty() {
            return 0;
        }
        if values.is_sorted() {
            return percentile_from_slice(values, p);
        }
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        percentile_from_slice(&sorted, p)
    }

    pub fn key_p50_us(&self) -> u128 {
        Self::sorted_percentile(&self.key_latencies_us, 50)
    }

    pub fn key_p99_us(&self) -> u128 {
        Self::sorted_percentile(&self.key_latencies_us, 99)
    }

    pub fn key_max_us(&self) -> u128 {
        self.key_max_us
    }

    pub fn sample_p50_us(&self) -> u128 {
        Self::sorted_percentile(&self.sample_total_us, 50)
    }

    pub fn sample_p99_us(&self) -> u128 {
        Self::sorted_percentile(&self.sample_total_us, 99)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L3TimingStats {
    pub backend: String,
    pub model_name: String,
    pub gpu_offload_active: bool,
    pub device_summary: String,
    pub warmup_ms: u64,
    pub calls: usize,
    pub p50_us: u128,
    pub p99_us: u128,
    pub mean_us: u128,
}

#[derive(Debug, Clone)]
pub struct EvalReport {
    pub dataset: String,
    pub by_cat: BTreeMap<Category, CategoryStats>,
    pub overall: CategoryStats,
    pub failures: Vec<SampleFailure>,
    pub l3_stats: Option<L3TimingStats>,
    /// 按 `notes` 标签分组的统计；无标签样本归入 [`UNTAGGED`]。
    pub by_tag: BTreeMap<String, CategoryStats>,
    /// 是否在报告中输出交互指标分节（`--interactive`）。
    pub show_interaction: bool,
    /// 按 TSV 顺序的逐行结果。
    pub rows: Vec<RowOutcome>,
}

/// 把逐行结果写成 TSV：`category  context  input  expected  ci_rank  top1  top5(" | " 分隔)`。
pub fn render_row_dump(report: &EvalReport) -> String {
    let clean = |s: &str| s.replace(['\t', '\n'], " ");
    let mut out = String::from("# category\tcontext\tinput\texpected\tci_rank\ttop1\ttop5\n");
    for r in &report.rows {
        let rank = r.ci_rank.map_or_else(|| "-".to_string(), |x| x.to_string());
        let top1 = r.top5.first().map(String::as_str).unwrap_or("");
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.category,
            clean(&r.context),
            clean(&r.input),
            clean(&r.expected),
            rank,
            clean(top1),
            r.top5.iter().map(|t| clean(t)).collect::<Vec<_>>().join(" | "),
        ));
    }
    out
}

/// Wilson 95% 置信区间（百分比），小样本下比正态近似可靠。
pub fn wilson_ci95(hits: usize, total: usize) -> (f64, f64) {
    if total == 0 {
        return (0.0, 0.0);
    }
    let (n, p, z) = (total as f64, hits as f64 / total as f64, 1.96f64);
    let denom = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denom;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denom;
    (100.0 * (center - half).max(0.0), 100.0 * (center + half).min(1.0))
}

pub const UNTAGGED: &str = "untagged";

/// 候选列表中首个命中任一参考答案的位置：`(忽略大小写, 大小写严格)`。
pub fn rank_against_references(texts: &[&str], references: &[String]) -> (Option<usize>, Option<usize>) {
    let ci = texts.iter().position(|t| {
        let t = t.trim();
        references.iter().any(|r| t.eq_ignore_ascii_case(r))
    });
    let cs = texts
        .iter()
        .position(|t| references.iter().any(|r| t.trim() == r));
    (ci, cs)
}

fn percentile_from_slice(sorted: &[u128], p: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let idx = ((n * p).div_ceil(100)).saturating_sub(1).min(n - 1);
    sorted[idx]
}

#[allow(dead_code)]
pub fn run_eval(dataset: &str, rows: &[EvalRow]) -> EvalReport {
    let mut session = EngineSession::new(
        StaticWordList::smoke_default(),
        NoopNgramModel,
        NoopNeuralModel,
    );
    run_eval_with_neural(dataset, rows, &mut session, None)
}

#[allow(dead_code)]
pub fn run_eval_with_session<L, N, M>(
    dataset: &str,
    rows: &[EvalRow],
    session: &mut EngineSession<L, N, M>,
) -> EvalReport
where
    L: LexiconProvider,
    N: NgramModel,
    M: NeuralSentenceModel,
{
    run_eval_with_neural(dataset, rows, session, None)
}

pub fn run_eval_with_neural<L, N, M>(
    dataset: &str,
    rows: &[EvalRow],
    session: &mut EngineSession<L, N, M>,
    neural_for_eval: Option<(&dyn NeuralSentenceModel, &NeuralRuntimeInfo)>,
) -> EvalReport
where
    L: LexiconProvider,
    N: NgramModel,
    M: NeuralSentenceModel,
{
    let mut by_cat: BTreeMap<Category, CategoryStats> = BTreeMap::new();
    let mut by_tag: BTreeMap<String, CategoryStats> = BTreeMap::new();
    let mut overall = CategoryStats::default();
    let mut failures = Vec::new();
    let mut l3_latencies_us: Vec<u128> = Vec::new();

    let mut row_outcomes: Vec<RowOutcome> = Vec::with_capacity(rows.len());
    // 单词模式（`space_commits = true`）：空格 / 标点会把首选上屏，最终文本 = 已上屏文本 + 剩余预编辑的候选。
    // 整句模式维持原口径（只看最终预编辑的候选），保证与旧报告逐行可比。
    let word_mode = session.config().space_commits;

    for row in rows {
        session.reset();
        session.set_context(row.context.clone(), "");
        let mut committed = String::new();

        let mut timing = SampleTiming {
            key_us: Vec::with_capacity(row.input.len()),
            key_max_us: 0,
        };
        let mut last_seq = None;
        let mut inter = SampleInteraction::default();
        let mut was_top1 = false;
        for (key_idx, ch) in row.input.chars().enumerate() {
            let key = if ch == ' ' {
                KeyInput::Space
            } else {
                KeyInput::Char(ch)
            };
            let start = Instant::now();
            let out = session.handle_key(key);
            let key_us = start.elapsed().as_micros();
            match out {
                EngineOutput::State(st) => {
                    // 预编辑为空时空格透传给宿主，宿主插入一个空格
                    if word_mode && st.passthrough && ch == ' ' {
                        committed.push(' ');
                    }
                    last_seq = Some(st.seq);
                }
                EngineOutput::Commit(c) => {
                    if word_mode {
                        committed.push_str(&c.text);
                    }
                    // 单词模式上屏后剩余预编辑仍可能继续组合（部分选词），L3 以当前 seq 合并
                    last_seq = (word_mode && !session.preedit().is_empty()).then(|| session.seq());
                }
            }
            timing.key_us.push(key_us);
            if session.preedit().chars().count() <= MAX_KEY_PREEDIT_CHARS {
                timing.key_max_us = timing.key_max_us.max(key_us);
            }
            // 排名检查放在计时之外，不计入单键延迟
            let texts = final_texts(session, &committed);
            let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
            let (rank, _) = rank_against_references(&texts, &row.references);
            inter.observe(key_idx + 1, rank, &mut was_top1);
        }
        inter.keys = row.input.chars().count();

        if let (Some((neural, _)), Some(seq)) = (neural_for_eval, last_seq) {
            let total_chars = session.preedit().chars().count();
            // 是否请求 L3 由当前模式的 ModeProfile 决定，与 animed 的调度口径一致
            if total_chars > 0 && session.wants_l3_rerank() {
                let cand_texts: Vec<String> = session
                    .candidates()
                    .iter()
                    .filter(|c| {
                        c.consumed_chars == total_chars
                            && !matches!(c.kind, CandidateKind::Raw | CandidateKind::Continuation)
                    })
                    .map(|c| c.text.clone())
                    .collect();
                if cand_texts.len() >= 2 {
                    let has_context = !row.context.trim().is_empty();
                    let has_multi_word = cand_texts.iter().any(|c| c.trim().contains(' '));
                    let l3_start = Instant::now();
                    // 单词模式下已上屏的词已推进引擎上文；整句模式上文即样本上文
                    let l3_context = if word_mode {
                        session.context().before.clone()
                    } else {
                        row.context.clone()
                    };
                    let reranked =
                        neural.rerank_sentences(&l3_context, session.preedit(), &cand_texts);
                    let l3_us = l3_start.elapsed().as_micros();
                    if has_context || has_multi_word || !reranked.is_empty() {
                        l3_latencies_us.push(l3_us);
                    }
                    if !reranked.is_empty() {
                        let _ = session.apply_neural_rerank(seq, &reranked, &[]);
                    }
                }
            }
        }

        let expected_exact = row.expected.trim();
        let owned_texts = final_texts(session, &committed);
        let texts: Vec<&str> = owned_texts.iter().map(String::as_str).collect();
        let (ci_rank, cs_rank) = rank_against_references(&texts, &row.references);
        // 最终排名以 L3 重排后的候选为准
        inter.final_rank = ci_rank;

        by_cat
            .entry(row.category)
            .or_default()
            .record(ci_rank, cs_rank, &timing);
        by_cat.get_mut(&row.category).expect("just inserted").interaction.add(&inter);
        by_tag
            .entry(row.tag.clone().unwrap_or_else(|| UNTAGGED.to_string()))
            .or_default()
            .record(ci_rank, cs_rank, &timing);
        if let Some(t) = by_tag.get_mut(&row.tag.clone().unwrap_or_else(|| UNTAGGED.to_string())) {
            t.interaction.add(&inter);
        }
        overall.record(ci_rank, cs_rank, &timing);
        overall.interaction.add(&inter);

        if ci_rank != Some(0) || cs_rank != Some(0) {
            failures.push(SampleFailure {
                category: row.category,
                context: row.context.clone(),
                input: row.input.clone(),
                expected: expected_exact.to_string(),
                top1_actual: owned_texts.first().cloned(),
                ci_rank,
                cs_rank,
            });
        }
        row_outcomes.push(RowOutcome {
            category: row.category,
            context: row.context.clone(),
            input: row.input.clone(),
            expected: expected_exact.to_string(),
            ci_rank,
            top5: owned_texts.iter().take(5).cloned().collect(),
        });
    }

    for stats in by_cat.values_mut().chain(by_tag.values_mut()) {
        stats.finalize();
    }
    overall.finalize();

    let l3_stats = neural_for_eval.map(|(_, info)| {
        l3_latencies_us.sort_unstable();
        let calls = l3_latencies_us.len();
        let mean_us = if calls == 0 {
            0
        } else {
            l3_latencies_us.iter().copied().sum::<u128>() / (calls as u128)
        };
        L3TimingStats {
            backend: info.backend_name.to_string(),
            model_name: info.model_name.clone(),
            gpu_offload_active: info.gpu_offload_active,
            device_summary: info.device_summary.clone(),
            warmup_ms: info.warmup_ms,
            calls,
            p50_us: percentile_from_slice(&l3_latencies_us, 50),
            p99_us: percentile_from_slice(&l3_latencies_us, 99),
            mean_us,
        }
    });

    EvalReport {
        dataset: dataset.to_string(),
        by_cat,
        overall,
        failures,
        l3_stats,
        by_tag,
        show_interaction: false,
        rows: row_outcomes,
    }
}

/// 最终候选文本：前缀为本样本内已上屏文本（整句模式恒为空）；预编辑已空时唯一结果就是已上屏文本。
fn final_texts<L, N, M>(session: &EngineSession<L, N, M>, committed: &str) -> Vec<String>
where
    L: LexiconProvider,
    N: NgramModel,
    M: NeuralSentenceModel,
{
    if committed.is_empty() {
        return session.candidates().iter().map(|c| c.text.clone()).collect();
    }
    if session.preedit().is_empty() {
        return vec![committed.to_string()];
    }
    session
        .candidates()
        .iter()
        .map(|c| format!("{committed}{}", c.text))
        .collect()
}
