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

    for row in rows {
        session.reset();
        session.set_context(row.context.clone(), "");

        let mut timing = SampleTiming {
            key_us: Vec::with_capacity(row.input.len()),
            key_max_us: 0,
        };
        let mut last_seq = None;
        for ch in row.input.chars() {
            let key = if ch == ' ' {
                KeyInput::Space
            } else {
                KeyInput::Char(ch)
            };
            let start = Instant::now();
            let out = session.handle_key(key);
            let key_us = start.elapsed().as_micros();
            match out {
                EngineOutput::State(st) => last_seq = Some(st.seq),
                EngineOutput::Commit(_) => last_seq = None,
            }
            timing.key_us.push(key_us);
            if session.preedit().chars().count() <= MAX_KEY_PREEDIT_CHARS {
                timing.key_max_us = timing.key_max_us.max(key_us);
            }
        }

        if let (Some((neural, _)), Some(seq)) = (neural_for_eval, last_seq) {
            let total_chars = session.preedit().chars().count();
            if total_chars > 0 {
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
                    let reranked =
                        neural.rerank_sentences(&row.context, session.preedit(), &cand_texts);
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
        let cands = session.candidates();
        let texts: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
        let (ci_rank, cs_rank) = rank_against_references(&texts, &row.references);

        by_cat
            .entry(row.category)
            .or_default()
            .record(ci_rank, cs_rank, &timing);
        by_tag
            .entry(row.tag.clone().unwrap_or_else(|| UNTAGGED.to_string()))
            .or_default()
            .record(ci_rank, cs_rank, &timing);
        overall.record(ci_rank, cs_rank, &timing);

        if ci_rank != Some(0) || cs_rank != Some(0) {
            failures.push(SampleFailure {
                category: row.category,
                context: row.context.clone(),
                input: row.input.clone(),
                expected: expected_exact.to_string(),
                top1_actual: cands.first().map(|c| c.text.clone()),
                ci_rank,
                cs_rank,
            });
        }
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
    }
}
