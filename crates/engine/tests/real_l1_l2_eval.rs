//! 真实 12 万词 L1 + 开源 OpenSLR 11 L2（2/3-gram Viterbi）端到端 7 类全场景集成评测与延迟门禁测试。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anime_engine::EngineSession;
use anime_lexicon::{Lexicon, LexiconProvider};
use anime_lm_neural::NoopNeuralModel;
use anime_lm_ngram::{NgramLanguageModel, NgramModel, UserStore};
use anime_protocol::KeyInput;

const ALL_CATEGORIES: [&str; 7] = [
    "exact",
    "prefix",
    "typo_simple",
    "typo_prefix",
    "typo_phonetic",
    "context_realword",
    "sentence",
];

#[derive(Default)]
struct CatMetric {
    total: usize,
    top1: usize,
    top3: usize,
    top5: usize,
    strict1: usize,
    last_key_us: Vec<u128>,
    misses: Vec<(String, String, String, String)>,
}

impl CatMetric {
    fn record(
        &mut self,
        context: &str,
        input: &str,
        expected: &str,
        rank_ci: Option<usize>,
        rank_cs: Option<usize>,
        top1_word: Option<&str>,
        last_key_us: u128,
    ) {
        self.total += 1;
        if rank_ci == Some(0) {
            self.top1 += 1;
        } else if self.misses.len() < 8 {
            self.misses.push((
                context.to_string(),
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
        self.last_key_us.push(last_key_us);
    }

    fn pct(hits: usize, total: usize) -> f64 {
        if total == 0 {
            0.0
        } else {
            (hits as f64) * 100.0 / (total as f64)
        }
    }

    fn percentile(samples: &[u128], p: usize) -> u128 {
        if samples.is_empty() {
            return 0;
        }
        let mut s = samples.to_vec();
        s.sort_unstable();
        let idx = ((s.len() * p).div_ceil(100))
            .saturating_sub(1)
            .min(s.len() - 1);
        s[idx]
    }
}

fn eval_7cats<L, N>(
    session: &mut EngineSession<L, N, NoopNeuralModel>,
    tsv_path: &Path,
    max_per_cat: Option<usize>,
) -> BTreeMap<&'static str, CatMetric>
where
    L: LexiconProvider,
    N: NgramModel,
{
    let raw = fs::read_to_string(tsv_path).expect("read eval TSV");
    let mut stats: BTreeMap<&'static str, CatMetric> = BTreeMap::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 5 {
            continue;
        }
        let cat_raw = parts[0].trim();
        let Some(&cat) = ALL_CATEGORIES.iter().find(|&&c| c == cat_raw) else {
            continue;
        };
        if let Some(cap) = max_per_cat {
            if stats.get(cat).is_some_and(|m| m.total >= cap) {
                continue;
            }
        }

        let context = parts[1];
        let input = parts[2].trim();
        let expected = parts[3].trim();
        let expected_lower = expected.to_ascii_lowercase();

        session.reset();
        session.set_context(context, "");

        let mut last_us = 0u128;
        for ch in input.chars() {
            let key = if ch == ' ' {
                KeyInput::Space
            } else {
                KeyInput::Char(ch)
            };
            let t0 = Instant::now();
            let _ = session.handle_key(key);
            last_us = t0.elapsed().as_micros();
        }

        let cands = session.candidates();
        let rank_ci = cands
            .iter()
            .position(|c| c.text.trim().to_ascii_lowercase() == expected_lower);
        let rank_cs = cands.iter().position(|c| c.text.trim() == expected);
        let top1 = cands.first().map(|c| c.text.as_str());

        stats
            .entry(cat)
            .or_default()
            .record(context, input, expected, rank_ci, rank_cs, top1, last_us);
    }

    stats
}

fn print_report(label: &str, stats: &BTreeMap<&'static str, CatMetric>) -> (u128, u128) {
    println!("\n=== End-to-End L1+L2 Eval: {label} ===");
    println!(
        "{:<18} {:>6} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8}",
        "category", "total", "top1%", "top3%", "top5%", "strict1%", "p50(us)", "p99(us)"
    );
    let mut all_lats = Vec::new();
    for cat in ALL_CATEGORIES {
        let Some(m) = stats.get(cat) else {
            continue;
        };
        all_lats.extend_from_slice(&m.last_key_us);
        println!(
            "{:<18} {:>6} {:>6.1}% {:>6.1}% {:>6.1}% {:>7.1}% {:>8} {:>8}",
            cat,
            m.total,
            CatMetric::pct(m.top1, m.total),
            CatMetric::pct(m.top3, m.total),
            CatMetric::pct(m.top5, m.total),
            CatMetric::pct(m.strict1, m.total),
            CatMetric::percentile(&m.last_key_us, 50),
            CatMetric::percentile(&m.last_key_us, 99),
        );
    }
    let p50 = CatMetric::percentile(&all_lats, 50);
    let p99 = CatMetric::percentile(&all_lats, 99);
    println!("Overall L1+L2 keystroke latency: p50={} us, p99={} us", p50, p99);
    for cat in ALL_CATEGORIES {
        if let Some(m) = stats.get(cat) {
            if !m.misses.is_empty() {
                println!("  [{cat}] sample top1 misses: {:?}", m.misses);
            }
        }
    }
    (p50, p99)
}

#[test]
fn real_l1_plus_l2_meets_7_category_accuracy_and_latency_gates() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lex_path = manifest_dir.join("../../data/compiled/lexicon.animelex.bin");
    let ngram_path = manifest_dir.join("../../data/compiled/ngram.animengr.bin");
    let core_tsv = manifest_dir.join("../../eval/benchmarks/core_v1.tsv");

    if !lex_path.is_file() || !ngram_path.is_file() {
        eprintln!(
            "SKIP real_l1_l2_eval: missing '{}' or '{}'",
            lex_path.display(),
            ngram_path.display()
        );
        return;
    }

    let t_lex = Instant::now();
    let lex = Lexicon::from_animelex_file(&lex_path).expect("load real ANIMELEX");
    let lex_ms = t_lex.elapsed().as_millis();

    let t_ng = Instant::now();
    let lm = Arc::new(
        NgramLanguageModel::from_animengr_file(&ngram_path, UserStore::in_memory())
            .expect("load real ANIMENGR"),
    );
    let ng_ms = t_ng.elapsed().as_millis();

    println!(
        "Loaded L1={} words ({} ms), L2 vocab={} 2g={} 3g={} ({} ms)",
        lex.len(),
        lex_ms,
        lm.vocab_size(),
        lm.bigram_count(),
        lm.trigram_count(),
        ng_ms
    );

    #[cfg(debug_assertions)]
    let max_per_cat = Some(20usize);
    #[cfg(not(debug_assertions))]
    let max_per_cat = None;

    let mut base_sess = EngineSession::new(lex.clone(), Arc::clone(&lm), NoopNeuralModel);
    let base_stats = eval_7cats(&mut base_sess, &core_tsv, max_per_cat);
    let (_base_p50, _base_p99) =
        print_report("eval/benchmarks/core_v1.tsv (Base L1 + OpenSLR L2)", &base_stats);

    let exact_top1 = CatMetric::pct(base_stats["exact"].top1, base_stats["exact"].total);
    let simple_top1 = CatMetric::pct(
        base_stats["typo_simple"].top1,
        base_stats["typo_simple"].total,
    );
    let phonetic_top3 = CatMetric::pct(
        base_stats["typo_phonetic"].top3,
        base_stats["typo_phonetic"].total,
    );
    #[cfg(debug_assertions)]
    {
        assert!(exact_top1 >= 88.0, "debug sample exact Top1 {exact_top1:.1}% below floor");
        assert!(
            simple_top1 >= 80.0,
            "debug sample typo_simple Top1 {simple_top1:.1}% below floor"
        );
        assert!(
            phonetic_top3 >= 70.0,
            "debug sample typo_phonetic Top3 {phonetic_top3:.1}% below floor"
        );
    }

    #[cfg(not(debug_assertions))]
    {
        use anime_lexicon::{DEFAULT_EXTENSION_ZIPF, LayeredLexicon};

        assert!(exact_top1 >= 98.0, "exact Top1 {exact_top1:.1}% < 98.0%");
        assert!(
            simple_top1 >= 85.0,
            "typo_simple Top1 {simple_top1:.1}% < 85.0%"
        );
        assert!(
            phonetic_top3 >= 75.0,
            "typo_phonetic Top3 {phonetic_top3:.1}% < 75.0%"
        );
        assert!(
            _base_p50 <= 2000,
            "L1+L2 core keystroke p50 {_base_p50} us > 2000 us"
        );
        assert!(
            _base_p99 <= 5000,
            "L1+L2 core keystroke p99 {_base_p99} us > 5000 us"
        );

        let mut layered = LayeredLexicon::new(lex);
        let tech_ext = Lexicon::from_tsv_str(
            "kubernetes\t4.8\nwayland\t4.8\ntypescript\t4.8\nfcitx\t4.5\nanimed\t4.5\n",
            DEFAULT_EXTENSION_ZIPF,
        )
        .expect("parse tech ext");
        layered.add_extension("tech_modern.tsv", tech_ext, 0.0);

        let mut layered_sess = EngineSession::new(layered, Arc::clone(&lm), NoopNeuralModel);
        let layered_stats = eval_7cats(&mut layered_sess, &core_tsv, None);
        let (lp50, lp99) = print_report(
            "eval/benchmarks/core_v1.tsv (Layered L1 + OpenSLR L2)",
            &layered_stats,
        );
        assert_eq!(
            layered_stats["exact"].top1, layered_stats["exact"].total,
            "Layered L1+L2 exact missed {}/{}",
            layered_stats["exact"].top1, layered_stats["exact"].total
        );
        assert!(lp50 <= 2000 && lp99 <= 5000);

        let large_tsv = manifest_dir.join("../../data/compiled/eval_large_open.tsv");
        if large_tsv.is_file() {
            let large_stats = eval_7cats(&mut base_sess, &large_tsv, None);
            let (gp50, gp99) = print_report(
                "data/compiled/eval_large_open.tsv (Base L1 + OpenSLR L2)",
                &large_stats,
            );
            let g_exact = CatMetric::pct(large_stats["exact"].top1, large_stats["exact"].total);
            let g_simple = CatMetric::pct(
                large_stats["typo_simple"].top1,
                large_stats["typo_simple"].total,
            );
            let g_phonetic = CatMetric::pct(
                large_stats["typo_phonetic"].top3,
                large_stats["typo_phonetic"].total,
            );
            assert!(g_exact >= 98.0, "large exact {g_exact:.1}% < 98.0%");
            assert!(
                g_simple >= 85.0,
                "large typo_simple {g_simple:.1}% < 85.0%"
            );
            assert!(
                g_phonetic >= 75.0,
                "large typo_phonetic Top3 {g_phonetic:.1}% < 75.0%"
            );
            assert!(gp50 <= 2000 && gp99 <= 5000);
        }
    }
}
