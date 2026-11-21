//! AnIME 离线评测入口：按 .agents/contracts/eval-schema.md 读取 TSV 并输出分场景 Top-K 与微秒延迟报告。

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::process;
use std::time::Instant;

use anime_engine::EngineSession;
use anime_lexicon::StaticWordList;
use anime_lm_neural::NoopNeuralModel;
use anime_lm_ngram::NoopNgramModel;
use anime_protocol::KeyInput;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EvalRow {
    category: String,
    context: String,
    input: String,
    expected: String,
    notes: String,
}

#[derive(Debug, Default)]
struct CategoryStats {
    total: usize,
    top1: usize,
    top3: usize,
    top5: usize,
    latencies_us: Vec<u128>,
}

impl CategoryStats {
    fn record(&mut self, rank: Option<usize>, latency_us: u128) {
        self.total += 1;
        if let Some(idx) = rank {
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
        self.latencies_us.push(latency_us);
    }

    fn pct(hits: usize, total: usize) -> f64 {
        if total == 0 {
            0.0
        } else {
            (hits as f64) * 100.0 / (total as f64)
        }
    }

    fn percentile(sorted: &[u128], p: usize) -> u128 {
        if sorted.is_empty() {
            return 0;
        }
        let idx = ((sorted.len() - 1) * p) / 100;
        sorted[idx]
    }
}

fn parse_tsv(content: &str) -> Result<Vec<EvalRow>, String> {
    let mut rows = Vec::new();
    for (line_no, raw) in content.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = raw.split('\t').collect();
        if parts.len() < 4 || parts.len() > 5 {
            return Err(format!(
                "line {}: expected 4 or 5 tab-separated columns, got {}",
                line_no + 1,
                parts.len()
            ));
        }
        rows.push(EvalRow {
            category: parts[0].trim().to_string(),
            context: parts[1].to_string(),
            input: parts[2].to_string(),
            expected: parts[3].trim().to_string(),
            notes: parts.get(4).map(|s| s.trim().to_string()).unwrap_or_default(),
        });
    }
    Ok(rows)
}

fn run_eval(rows: &[EvalRow]) -> (BTreeMap<String, CategoryStats>, CategoryStats) {
    let mut session = EngineSession::new(
        StaticWordList::smoke_default(),
        NoopNgramModel,
        NoopNeuralModel,
    );
    let mut by_cat: BTreeMap<String, CategoryStats> = BTreeMap::new();
    let mut overall = CategoryStats::default();

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

        let expected_norm = row.expected.trim().to_ascii_lowercase();
        let rank = session
            .candidates()
            .iter()
            .position(|c| c.text.trim().to_ascii_lowercase() == expected_norm);

        by_cat
            .entry(row.category.clone())
            .or_default()
            .record(rank, elapsed_us);
        overall.record(rank, elapsed_us);
    }

    (by_cat, overall)
}

fn print_report(path: &str, by_cat: &mut BTreeMap<String, CategoryStats>, overall: &mut CategoryStats) {
    println!("AnIME Offline Eval Report ({path})");
    println!(
        "{:<18} {:>5} {:>8} {:>8} {:>8} {:>10} {:>10}",
        "category", "total", "top1%", "top3%", "top5%", "p50(us)", "p99(us)"
    );
    println!("{}", "-".repeat(73));

    for (cat, stats) in by_cat.iter_mut() {
        stats.latencies_us.sort_unstable();
        println!(
            "{:<18} {:>5} {:>7.1}% {:>7.1}% {:>7.1}% {:>10} {:>10}",
            cat,
            stats.total,
            CategoryStats::pct(stats.top1, stats.total),
            CategoryStats::pct(stats.top3, stats.total),
            CategoryStats::pct(stats.top5, stats.total),
            CategoryStats::percentile(&stats.latencies_us, 50),
            CategoryStats::percentile(&stats.latencies_us, 99),
        );
    }

    println!("{}", "-".repeat(73));
    overall.latencies_us.sort_unstable();
    println!(
        "{:<18} {:>5} {:>7.1}% {:>7.1}% {:>7.1}% {:>10} {:>10}",
        "TOTAL",
        overall.total,
        CategoryStats::pct(overall.top1, overall.total),
        CategoryStats::pct(overall.top3, overall.total),
        CategoryStats::pct(overall.top5, overall.total),
        CategoryStats::percentile(&overall.latencies_us, 50),
        CategoryStats::percentile(&overall.latencies_us, 99),
    );
}

fn main() {
    let tsv_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "eval/fixtures/smoke.tsv".to_string());
    let content = match fs::read_to_string(&tsv_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("failed to read eval TSV '{tsv_path}': {err}");
            process::exit(1);
        }
    };
    let rows = match parse_tsv(&content) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("invalid eval TSV '{tsv_path}': {err}");
            process::exit(1);
        }
    };
    if rows.is_empty() {
        eprintln!("no eval cases found in '{tsv_path}'");
        process::exit(1);
    }

    let (mut by_cat, mut overall) = run_eval(&rows);
    print_report(&tsv_path, &mut by_cat, &mut overall);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_run_smoke_fixture() {
        let sample = "\
# comment
exact\t\tapple\tapple\tknown word
typo_simple\t\tappie\tapple\tsimple typo
context_realword\tgo \tform here\tfrom here\tcontext
";
        let rows = parse_tsv(sample).expect("parse sample tsv");
        assert_eq!(rows.len(), 3);
        let (by_cat, overall) = run_eval(&rows);
        assert_eq!(overall.total, 3);
        assert_eq!(by_cat["exact"].top1, 1);
    }
}
