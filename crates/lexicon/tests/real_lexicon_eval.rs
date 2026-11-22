//! 真实 12 万词 `ANIMELEX` 词典集成评测与微秒延迟门禁测试。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use anime_lexicon::{
    DEFAULT_EXTENSION_ZIPF, LayeredLexicon, Lexicon, LexiconProvider, QueryOptions,
};

#[derive(Default)]
struct CatMetric {
    total: usize,
    top1: usize,
    top3: usize,
    top5: usize,
    strict1: usize,
    latencies_us: Vec<u128>,
    misses: Vec<(String, String, String)>,
}

impl CatMetric {
    fn record(
        &mut self,
        input: &str,
        expected: &str,
        rank_ci: Option<usize>,
        rank_cs: Option<usize>,
        top1_word: Option<&str>,
        latency_us: u128,
    ) {
        self.total += 1;
        if rank_ci == Some(0) {
            self.top1 += 1;
        } else if self.misses.len() < 12 {
            self.misses.push((
                input.to_string(),
                expected.to_string(),
                top1_word.unwrap_or("<none>").to_string(),
            ));
        }
        if matches!(rank_ci, Some(r) if r < 3) {
            self.top3 += 1;
        }
        if matches!(rank_ci, Some(r) if r < 5) {
            self.top5 += 1;
        }
        if rank_cs == Some(0) {
            self.strict1 += 1;
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

    fn percentile(&self, p: usize) -> u128 {
        if self.latencies_us.is_empty() {
            return 0;
        }
        let mut s = self.latencies_us.clone();
        s.sort_unstable();
        let idx = ((s.len() * p).div_ceil(100)).saturating_sub(1).min(s.len() - 1);
        s[idx]
    }
}

fn eval_word_tsv<L: LexiconProvider>(lex: &L, tsv_path: &Path) -> BTreeMap<String, CatMetric> {
    let raw = fs::read_to_string(tsv_path).expect("read eval TSV");
    let opts = QueryOptions::default();
    let mut stats: BTreeMap<String, CatMetric> = BTreeMap::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 5 {
            continue;
        }
        let cat = parts[0].trim();
        if !matches!(
            cat,
            "exact" | "prefix" | "typo_simple" | "typo_prefix" | "typo_phonetic"
        ) {
            continue;
        }
        let input = parts[2].trim();
        let expected = parts[3].trim();
        // eval-schema §1：`||` 分隔的多参考答案任一命中即算命中
        let refs: Vec<&str> = expected.split("||").map(str::trim).collect();

        let t0 = Instant::now();
        let hits = lex.query(input, &opts);
        let us = t0.elapsed().as_micros();

        let rank_ci = hits
            .iter()
            .position(|c| refs.iter().any(|r| c.word.eq_ignore_ascii_case(r)));
        let rank_cs = hits.iter().position(|c| refs.iter().any(|r| c.word == *r));
        let top1 = hits.first().map(|c| c.word.as_str());

        stats
            .entry(cat.to_string())
            .or_default()
            .record(input, expected, rank_ci, rank_cs, top1, us);
    }

    stats
}

fn print_summary(label: &str, stats: &BTreeMap<String, CatMetric>) -> (u128, u128) {
    println!("\n=== L1 Real Lexicon Eval: {label} ===");
    println!(
        "{:<16} {:>6} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8}",
        "category", "total", "top1%", "top3%", "top5%", "strict1%", "p50(us)", "p99(us)"
    );
    let mut all_lats = Vec::new();
    for (cat, m) in stats {
        all_lats.extend_from_slice(&m.latencies_us);
        println!(
            "{:<16} {:>6} {:>6.1}% {:>6.1}% {:>6.1}% {:>7.1}% {:>8} {:>8}",
            cat,
            m.total,
            CatMetric::pct(m.top1, m.total),
            CatMetric::pct(m.top3, m.total),
            CatMetric::pct(m.top5, m.total),
            CatMetric::pct(m.strict1, m.total),
            m.percentile(50),
            m.percentile(99),
        );
    }
    all_lats.sort_unstable();
    let p50 = if all_lats.is_empty() {
        0
    } else {
        all_lats[((all_lats.len() * 50).div_ceil(100)).saturating_sub(1)]
    };
    let p99 = if all_lats.is_empty() {
        0
    } else {
        all_lats[((all_lats.len() * 99).div_ceil(100)).saturating_sub(1)]
    };
    println!("Overall word query latency: p50={} us, p99={} us", p50, p99);
    for (cat, m) in stats {
        if !m.misses.is_empty() {
            println!("  [{cat}] sample top1 misses: {:?}", &m.misses[..m.misses.len().min(6)]);
        }
    }
    (p50, p99)
}

#[test]
fn real_animelex_meets_phase1_accuracy_and_latency_gates() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin_path = manifest_dir.join("../../data/compiled/lexicon.animelex.bin");
    let core_tsv = manifest_dir.join("../../eval/benchmarks/core_v1.tsv");

    if !bin_path.is_file() {
        eprintln!(
            "SKIP real_lexicon_eval: '{}' not found (run scripts/data_pipeline/build_lexicon.py first)",
            bin_path.display()
        );
        return;
    }

    let load_t0 = Instant::now();
    let lex = Lexicon::from_animelex_file(&bin_path).expect("load real ANIMELEX binary");
    let load_ms = load_t0.elapsed().as_millis();
    println!(
        "Loaded {} words and {} typo keys from {} in {} ms",
        lex.len(),
        lex.misspelling_count(),
        bin_path.display(),
        load_ms
    );
    assert!(lex.len() >= 10_000);

    let core_stats = eval_word_tsv(&lex, &core_tsv);
    let (_core_p50, _core_p99) = print_summary("eval/benchmarks/core_v1.tsv", &core_stats);

    let exact_top1 = CatMetric::pct(core_stats["exact"].top1, core_stats["exact"].total);
    let simple_top1 = CatMetric::pct(
        core_stats["typo_simple"].top1,
        core_stats["typo_simple"].total,
    );
    let phonetic_top3 = CatMetric::pct(
        core_stats["typo_phonetic"].top3,
        core_stats["typo_phonetic"].total,
    );

    assert!(
        exact_top1 >= 98.0,
        "exact Top1 {exact_top1:.1}% below 98.0% gate"
    );
    assert!(
        simple_top1 >= 85.0,
        "typo_simple Top1 {simple_top1:.1}% below 85.0% gate"
    );
    assert!(
        phonetic_top3 >= 75.0,
        "typo_phonetic Top3 {phonetic_top3:.1}% below 75.0% gate"
    );

    #[cfg(not(debug_assertions))]
    {
        assert!(
            _core_p99 <= 1000,
            "release L1 query p99 latency {_core_p99} us exceeds 1000 us (< 1 ms) budget"
        );

        let large_tsv = manifest_dir.join("../../data/compiled/eval_large_open.tsv");
        if large_tsv.is_file() {
            let large_stats = eval_word_tsv(&lex, &large_tsv);
            let (_lp50, lp99) = print_summary("data/compiled/eval_large_open.tsv", &large_stats);
            let l_exact = CatMetric::pct(large_stats["exact"].top1, large_stats["exact"].total);
            let l_simple = CatMetric::pct(
                large_stats["typo_simple"].top1,
                large_stats["typo_simple"].total,
            );
            let l_phonetic = CatMetric::pct(
                large_stats["typo_phonetic"].top3,
                large_stats["typo_phonetic"].total,
            );
            assert!(l_exact >= 98.0, "large exact Top1 {l_exact:.1}% < 98.0%");
            assert!(
                l_simple >= 85.0,
                "large typo_simple Top1 {l_simple:.1}% < 85.0%"
            );
            assert!(
                l_phonetic >= 75.0,
                "large typo_phonetic Top3 {l_phonetic:.1}% < 75.0%"
            );
            assert!(
                lp99 <= 1000,
                "large release L1 query p99 latency {lp99} us exceeds 1000 us (< 1 ms) budget"
            );
        }
    }
}

#[test]
fn layered_lexicon_extends_real_animelex_within_latency_budget() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin_path = manifest_dir.join("../../data/compiled/lexicon.animelex.bin");
    let core_tsv = manifest_dir.join("../../eval/benchmarks/core_v1.tsv");

    if !bin_path.is_file() {
        eprintln!(
            "SKIP layered real_lexicon_eval: '{}' not found",
            bin_path.display()
        );
        return;
    }

    let mut layered =
        LayeredLexicon::from_animelex_file(&bin_path).expect("load real ANIMELEX binary");
    let tech_ext = Lexicon::from_tsv_str(
        "kubernetes\t4.8\nwayland\t4.8\ntypescript\t4.8\nfcitx\t4.5\nanimed\t4.5\n",
        DEFAULT_EXTENSION_ZIPF,
    )
    .expect("parse tech_modern.tsv");
    layered.add_extension("tech_modern.tsv", tech_ext, 0.0);

    assert!(layered.learn_word("Antigravity", 3));
    assert!(layered.learn_word("Hyprland", 2));
    assert_eq!(layered.dynamic_word_count(), 2);

    let stats = eval_word_tsv(&layered, &core_tsv);
    let (_layered_p50, _layered_p99) = print_summary(
        "eval/benchmarks/core_v1.tsv (LayeredLexicon 3-layer)",
        &stats,
    );

    assert_eq!(
        stats["exact"].top1, stats["exact"].total,
        "layered exact Top1 missed some words ({}/{})",
        stats["exact"].top1, stats["exact"].total
    );

    let simple_top1 = CatMetric::pct(stats["typo_simple"].top1, stats["typo_simple"].total);
    let phonetic_top3 = CatMetric::pct(stats["typo_phonetic"].top3, stats["typo_phonetic"].total);
    assert!(
        simple_top1 >= 85.0,
        "layered typo_simple Top1 {simple_top1:.1}% below 85.0% gate"
    );
    assert!(
        phonetic_top3 >= 75.0,
        "layered typo_phonetic Top3 {phonetic_top3:.1}% below 75.0% gate"
    );

    let opts = QueryOptions::default();
    assert_eq!(layered.query("kubern", &opts)[0].word, "kubernetes");
    assert_eq!(layered.query("kubrenet", &opts)[0].word, "kubernetes");
    assert_eq!(layered.query("waylnd", &opts)[0].word, "wayland");
    assert_eq!(layered.query("antigr", &opts)[0].word, "Antigravity");
    assert_eq!(layered.query("hyprlnd", &opts)[0].word, "Hyprland");

    #[cfg(not(debug_assertions))]
    {
        assert!(
            _layered_p99 <= 1000,
            "release LayeredLexicon p99 latency {_layered_p99} us exceeds 1000 us (< 1 ms) budget"
        );
    }
}
