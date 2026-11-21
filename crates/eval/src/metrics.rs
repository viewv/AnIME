//! 评测执行器与统计指标：同时统计忽略大小写与大小写严格 Top-1/3/5 及微秒分位延迟。

use std::collections::BTreeMap;
use std::time::Instant;

use anime_engine::EngineSession;
use anime_lexicon::{LexiconProvider, StaticWordList};
use anime_lm_neural::{NeuralSentenceModel, NoopNeuralModel};
use anime_lm_ngram::{NgramModel, NoopNgramModel};
use anime_protocol::KeyInput;

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
    pub latencies_us: Vec<u128>,
}

impl CategoryStats {
    pub fn record(&mut self, ci_rank: Option<usize>, cs_rank: Option<usize>, latency_us: u128) {
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
        self.latencies_us.push(latency_us);
    }

    pub fn finalize(&mut self) {
        self.latencies_us.sort_unstable();
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

    pub fn percentile_us(&self, p: usize) -> u128 {
        if self.latencies_us.is_empty() {
            return 0;
        }
        let mut sorted = self.latencies_us.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        let idx = ((n * p).div_ceil(100)).saturating_sub(1).min(n - 1);
        sorted[idx]
    }

    pub fn p50_us(&self) -> u128 {
        self.percentile_us(50)
    }

    pub fn p99_us(&self) -> u128 {
        self.percentile_us(99)
    }
}

#[derive(Debug, Clone)]
pub struct EvalReport {
    pub dataset: String,
    pub by_cat: BTreeMap<Category, CategoryStats>,
    pub overall: CategoryStats,
    pub failures: Vec<SampleFailure>,
}

pub fn run_eval(dataset: &str, rows: &[EvalRow]) -> EvalReport {
    let mut session = EngineSession::new(
        StaticWordList::smoke_default(),
        NoopNgramModel,
        NoopNeuralModel,
    );
    run_eval_with_session(dataset, rows, &mut session)
}

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
    let mut by_cat: BTreeMap<Category, CategoryStats> = BTreeMap::new();
    let mut overall = CategoryStats::default();
    let mut failures = Vec::new();

    for row in rows {
        session.reset();
        session.set_context(row.context.clone(), "");

        let start = Instant::now();
        for ch in row.input.chars() {
            let key = if ch == ' ' {
                KeyInput::Space
            } else {
                KeyInput::Char(ch)
            };
            let _ = session.handle_key(key);
        }
        let elapsed_us = start.elapsed().as_micros();

        let expected_exact = row.expected.trim();
        let expected_lower = expected_exact.to_ascii_lowercase();
        let cands = session.candidates();

        let ci_rank = cands
            .iter()
            .position(|c| c.text.trim().to_ascii_lowercase() == expected_lower);
        let cs_rank = cands.iter().position(|c| c.text.trim() == expected_exact);

        by_cat
            .entry(row.category)
            .or_default()
            .record(ci_rank, cs_rank, elapsed_us);
        overall.record(ci_rank, cs_rank, elapsed_us);

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

    for stats in by_cat.values_mut() {
        stats.finalize();
    }
    overall.finalize();

    EvalReport {
        dataset: dataset.to_string(),
        by_cat,
        overall,
        failures,
    }
}
