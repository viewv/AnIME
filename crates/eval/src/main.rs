//! AnIME 离线评测入口：按 .agents/contracts/eval-schema.md 读取 TSV 并输出分场景 Top-K（含大小写敏感/忽略）、延迟与门禁报告。

mod cli;
mod gate;
mod metrics;
mod report;
mod tsv;

use std::env;
use std::fs;
use std::path::Path;
use std::process;
use std::sync::Arc;

use anime_engine::EngineSession;
use anime_lexicon::{
    ConfusableCandidate, LayeredLexicon, Lexicon, LexiconCandidate, LexiconProvider, QueryOptions,
    StaticWordList,
};
use anime_lm_neural::{NeuralModelConfig, NeuralRuntimeInfo, NeuralSentenceModel, NoopNeuralModel, load_neural_model};
use anime_lm_ngram::{NgramLanguageModel, NgramModel, NoopNgramModel, UserStore};
use cli::{OutputFormat, parse_args, usage_text};
use gate::{evaluate_gates, parse_gate_spec};
use metrics::{run_eval_with_neural};
use report::{render_json, render_markdown, render_text};
use tsv::parse_tsv;

enum EvalLexicon {
    Smoke(StaticWordList),
    Compiled(LayeredLexicon),
}

impl LexiconProvider for EvalLexicon {
    fn query(&self, raw: &str, opts: &QueryOptions) -> Vec<LexiconCandidate> {
        match self {
            Self::Smoke(s) => s.query(raw, opts),
            Self::Compiled(l) => l.query(raw, opts),
        }
    }

    fn contextual_confusables(&self, raw: &str, limit: usize) -> Vec<ConfusableCandidate> {
        match self {
            Self::Smoke(s) => s.contextual_confusables(raw, limit),
            Self::Compiled(l) => l.contextual_confusables(raw, limit),
        }
    }

    fn segment_runon(&self, raw: &str) -> Option<Vec<String>> {
        match self {
            Self::Smoke(s) => s.segment_runon(raw),
            Self::Compiled(l) => l.segment_runon(raw),
        }
    }
}

enum EvalNgram {
    Noop(NoopNgramModel),
    Static(NgramLanguageModel),
}

impl NgramModel for EvalNgram {
    fn score_word(&self, prev_word: Option<&str>, word: &str) -> f32 {
        match self {
            Self::Noop(n) => n.score_word(prev_word, word),
            Self::Static(n) => n.score_word(prev_word, word),
        }
    }

    fn score_in_context(&self, context_before: &str, word: &str) -> f32 {
        match self {
            Self::Noop(n) => n.score_in_context(context_before, word),
            Self::Static(n) => n.score_in_context(context_before, word),
        }
    }

    fn decode_lattice(
        &self,
        context_before: &str,
        slots: &[anime_lm_ngram::LatticeSlot],
        limit: usize,
    ) -> Vec<anime_lm_ngram::DecodedSentence> {
        match self {
            Self::Noop(n) => n.decode_lattice(context_before, slots, limit),
            Self::Static(n) => n.decode_lattice(context_before, slots, limit),
        }
    }

    fn learn_commit(&self, context_before: &str, committed_text: &str, privacy: bool) {
        match self {
            Self::Noop(n) => n.learn_commit(context_before, committed_text, privacy),
            Self::Static(n) => n.learn_commit(context_before, committed_text, privacy),
        }
    }
}

fn write_output_file(path: &str, content: &str) -> Result<(), String> {
    let target = Path::new(path);
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create directory '{}': {e}", parent.display()))?;
        }
    }
    fs::write(target, content).map_err(|e| format!("failed to write '{path}': {e}"))
}

fn main() {
    let args = match parse_args(env::args().skip(1)) {
        Ok(a) => a,
        Err(err) => {
            eprintln!("argument error: {err}\n\n{}", usage_text());
            process::exit(1);
        }
    };

    if args.help {
        println!("{}", usage_text());
        return;
    }

    let mut gate_rules = Vec::new();
    for spec in &args.gates {
        match parse_gate_spec(spec, args.strict_gate) {
            Ok(rules) => gate_rules.extend(rules),
            Err(err) => {
                eprintln!("invalid --gate specification: {err}");
                process::exit(1);
            }
        }
    }

    let content = match fs::read_to_string(&args.tsv_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("failed to read eval TSV '{}': {err}", args.tsv_path);
            process::exit(1);
        }
    };

    let mut rows = match parse_tsv(&content) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("invalid eval TSV '{}': {err}", args.tsv_path);
            process::exit(1);
        }
    };
    if let Some(limit) = args.max_samples {
        rows.truncate(limit);
    }
    if rows.is_empty() {
        eprintln!("no eval cases found in '{}'", args.tsv_path);
        process::exit(1);
    }

    let lex = match &args.lexicon_path {
        Some(path) => match Lexicon::from_animelex_file(Path::new(path)) {
            Ok(lex) => EvalLexicon::Compiled(LayeredLexicon::new(lex)),
            Err(err) => {
                eprintln!("failed loading lexicon '{path}': {err}");
                process::exit(1);
            }
        },
        None => EvalLexicon::Smoke(StaticWordList::smoke_default()),
    };

    let ngram = match &args.ngram_path {
        Some(path) => match anime_lm_ngram::load_animengr_file(Path::new(path)) {
            Ok(data) => EvalNgram::Static(NgramLanguageModel::from_data(data, UserStore::in_memory())),
            Err(err) => {
                eprintln!("failed loading n-gram '{path}': {err}");
                process::exit(1);
            }
        },
        None => EvalNgram::Noop(NoopNgramModel),
    };

    let loaded_neural: Option<(Arc<dyn NeuralSentenceModel>, NeuralRuntimeInfo)> =
        if let Some(ref model_path) = args.model_path {
            let mut ncfg = NeuralModelConfig::with_model_path(model_path);
            ncfg.n_gpu_layers = args.gpu_layers;
            if let Some(w) = args.rerank_weight {
                ncfg.rerank_weight = w;
            }
            match load_neural_model(ncfg) {
                Ok(pair) => Some(pair),
                Err(err) => {
                    eprintln!("failed loading L3 GGUF model '{model_path}': {err}");
                    process::exit(1);
                }
            }
        } else {
            None
        };

    // Eval 流程由 `run_eval_with_neural` 在同线程显式测算 L3 rerank 耗时并写入报告，
    // 因此 session 内置 `NoopNeuralModel` 不受 L3 调度影响，避免重复推理。
    let mut session = EngineSession::new(lex, ngram, NoopNeuralModel);
    let neural_ref = loaded_neural
        .as_ref()
        .map(|(m, info)| (m.as_ref(), info));
    let mut report = run_eval_with_neural(&args.tsv_path, &rows, &mut session, neural_ref);
    let gate_summary = if gate_rules.is_empty() {
        None
    } else {
        Some(evaluate_gates(
            &report,
            &gate_rules,
            args.gates.clone(),
            args.strict_gate,
        ))
    };
    // 标签门禁已在上面求值；未要求分组时不输出标签分节，保持默认报告精简
    if !args.group_by_tag {
        report.by_tag.clear();
    }

    if let Some(ref json_path) = args.json_out {
        let json_body = render_json(&report, gate_summary.as_ref(), args.show_failures);
        if let Err(err) = write_output_file(json_path, &json_body) {
            eprintln!("{err}");
            process::exit(1);
        }
    }

    if let Some(ref md_path) = args.md_out {
        let md_body = render_markdown(&report, gate_summary.as_ref(), args.show_failures);
        if let Err(err) = write_output_file(md_path, &md_body) {
            eprintln!("{err}");
            process::exit(1);
        }
    }

    let stdout_body = match args.format {
        OutputFormat::Text => render_text(&report, gate_summary.as_ref(), args.show_failures),
        OutputFormat::Json => render_json(&report, gate_summary.as_ref(), args.show_failures),
        OutputFormat::Markdown => render_markdown(&report, gate_summary.as_ref(), args.show_failures),
    };
    print!("{stdout_body}");

    if let Some(ref g) = gate_summary {
        if !g.passed {
            process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use anime_engine::EngineSession;
    use anime_lexicon::{LexiconCandidate, LexiconMatchKind, LexiconProvider, QueryOptions};
    use anime_lm_neural::NoopNeuralModel;
    use anime_lm_ngram::NoopNgramModel;

    use crate::cli::{OutputFormat, parse_args};
    use crate::gate::{evaluate_gates, parse_gate_spec};
    use crate::metrics::{
        CategoryStats, EvalReport, SampleTiming, run_eval, run_eval_with_session,
    };
    use crate::report::{render_json, render_markdown, render_text};
    use crate::tsv::{Category, parse_tsv};

    struct LowercaseMockLexicon;

    impl LexiconProvider for LowercaseMockLexicon {
        fn query(&self, raw_token: &str, _opts: &QueryOptions) -> Vec<LexiconCandidate> {
            vec![LexiconCandidate {
                word: raw_token.to_ascii_lowercase(),
                kind: LexiconMatchKind::Exact,
                edit_cost: 0.0,
                log_freq: -0.1,
                score: -0.1,
            }]
        }
    }

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
        let report = run_eval("inline", &rows);
        assert_eq!(report.overall.total, 3);
        assert_eq!(report.by_cat[&Category::Exact].top1, 1);
        assert_eq!(report.by_cat[&Category::Exact].strict_top1, 1);
    }

    #[test]
    fn parse_tsv_rejects_bad_columns_bom_and_unknown_category() {
        assert!(parse_tsv("\u{feff}exact\t\tapple\tapple\tok").is_err());
        assert!(parse_tsv("exact\t\tapple\tapple").is_err());
        assert!(parse_tsv("unknown_cat\t\tapple\tapple\tnotes").is_err());
        assert!(parse_tsv("exact\t\t\tapple\tempty input").is_err());
        assert!(parse_tsv("exact\t\tapple\t\tempty expected").is_err());
    }

    #[test]
    fn validates_checked_in_smoke_and_core_v1_benchmarks() {
        let smoke_rows =
            parse_tsv(include_str!("../../../eval/fixtures/smoke.tsv")).expect("smoke.tsv valid");
        assert_eq!(smoke_rows.len(), 14);

        let core_rows =
            parse_tsv(include_str!("../../../eval/benchmarks/core_v1.tsv")).expect("core_v1.tsv valid");
        assert!(
            core_rows.len() >= 1000,
            "core_v1.tsv should contain >= 1000 cases, got {}",
            core_rows.len()
        );

        let mut counts: BTreeMap<Category, usize> = BTreeMap::new();
        let mut seen = HashSet::new();
        for row in &core_rows {
            *counts.entry(row.category).or_insert(0) += 1;
            let inserted = seen.insert((row.category, row.context.clone(), row.input.clone()));
            assert!(
                inserted,
                "duplicate benchmark key: ({}, {:?}, {:?})",
                row.category, row.context, row.input
            );

            let inp_l = row.input.to_ascii_lowercase();
            let exp_l = row.expected.to_ascii_lowercase();
            match row.category {
                Category::Exact => assert_eq!(row.input, row.expected, "exact must match verbatim"),
                Category::Prefix => assert!(
                    exp_l.starts_with(&inp_l) && inp_l.len() < exp_l.len(),
                    "prefix row must be strict prefix: {:?} -> {:?}",
                    row.input,
                    row.expected
                ),
                Category::TypoPrefix => assert!(
                    !exp_l.starts_with(&inp_l) && inp_l.len() < exp_l.len(),
                    "typo_prefix row must contain typo and be shorter than expected: {:?} -> {:?}",
                    row.input,
                    row.expected
                ),
                Category::TypoSimple | Category::TypoPhonetic => assert_ne!(
                    inp_l, exp_l,
                    "typo row must differ from expected: {:?} -> {:?}",
                    row.input, row.expected
                ),
                Category::ContextRealword | Category::Sentence => {}
            }
        }

        for cat in Category::ALL {
            assert!(
                counts.get(&cat).copied().unwrap_or(0) >= 150,
                "category {} has too few cases: {:?}",
                cat,
                counts.get(&cat)
            );
        }
        assert!(counts[&Category::TypoPhonetic] >= 150);
        assert!(counts[&Category::Sentence] >= 150);
    }

    #[test]
    fn tracks_case_insensitive_vs_strict_accuracy() {
        let rows = parse_tsv(
            "exact\t\tapple\tapple\tlower match\n\
             exact\t\ti\tI\tcapitalization mismatch\n",
        )
        .expect("parse");
        let mut session = EngineSession::new(LowercaseMockLexicon, NoopNgramModel, NoopNeuralModel);
        let report = run_eval_with_session("case-test", &rows, &mut session);
        let exact = &report.by_cat[&Category::Exact];
        assert_eq!(exact.total, 2);
        assert_eq!(exact.top1, 2);
        assert_eq!(exact.strict_top1, 1);
        assert!((exact.top1_pct() - 100.0).abs() < 0.01);
        assert!((exact.strict_top1_pct() - 50.0).abs() < 0.01);
        // 第 2 条大小写未精确命中时应进入 failures 列表
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].expected, "I");
        assert_eq!(report.failures[0].ci_rank, Some(0));
        assert_eq!(report.failures[0].cs_rank, None);
    }

    fn keys(us: &[u128]) -> SampleTiming {
        SampleTiming {
            key_us: us.to_vec(),
            key_max_us: us.iter().copied().max().unwrap_or(0),
        }
    }

    #[test]
    fn per_key_latency_is_separated_from_sample_total() {
        let mut stats = CategoryStats::default();
        // 两条样本：逐键耗时都远低于各自累计总耗时
        stats.record(Some(0), Some(0), &keys(&[100, 200, 300]));
        stats.record(Some(0), Some(0), &keys(&[400, 500]));
        stats.finalize();
        assert_eq!(stats.key_latencies_us.len(), 5);
        assert_eq!(stats.key_p50_us(), 300);
        assert_eq!(stats.key_p99_us(), 500);
        assert_eq!(stats.key_max_us(), 500);
        assert_eq!(stats.sample_p50_us(), 600);
        assert_eq!(stats.sample_p99_us(), 900);

        // 超长预编辑上的按键不计入 key_max（由 run_eval 在采样时过滤）
        let long = SampleTiming {
            key_us: vec![100, 90_000],
            key_max_us: 100,
        };
        let mut s2 = CategoryStats::default();
        s2.record(Some(0), Some(0), &long);
        assert_eq!(s2.key_max_us(), 100);
        assert_eq!(s2.key_p99_us(), 90_000);
    }

    #[test]
    fn phase3_gate_uses_per_key_budget_and_legacy_alias() {
        let rules = parse_gate_spec("phase3", false).expect("phase3");
        let latency: Vec<_> = rules
            .iter()
            .filter(|r| r.metric.unit() == "us")
            .map(|r| (r.metric.as_str(), r.threshold))
            .collect();
        assert_eq!(
            latency,
            vec![
                ("key_p50_us", 3000.0),
                ("key_p99_us", 10_000.0),
                ("key_max_us", 16_000.0)
            ]
        );
        assert!(rules.iter().any(|r| r.label == "context_realword.top3"));

        let alias = parse_gate_spec("total.p99_us<=10", false).expect("alias");
        assert_eq!(alias[0].metric.as_str(), "key_p99_us");
        assert!(parse_gate_spec("total.key_max_us<=16000", false).is_ok());
    }

    #[test]
    fn multi_reference_expected_and_notes_tags() {
        use crate::metrics::rank_against_references;
        use crate::tsv::{parse_tag, split_references};

        let rows = parse_tsv(
            "typo_prefix\t\tgrenad\tgrenade || grenades\tnoise:adjacent_key truncated\n\
             sentence\t\tthe cat\tthe cat\tnegative:clean\n\
             exact\t\tapple\tapple\tfree text: not a tag\n",
        )
        .expect("parse");
        assert_eq!(rows[0].references, vec!["grenade", "grenades"]);
        assert_eq!(rows[0].tag.as_deref(), Some("noise:adjacent_key"));
        assert_eq!(rows[1].tag.as_deref(), Some("negative:clean"));
        assert_eq!(rows[2].tag, None);
        assert!(parse_tsv("exact\t\ta\ta || \tx\n").is_err());
        assert!(split_references("a||b").is_ok());
        assert_eq!(parse_tag("Noise:X y"), None);

        // 任一参考命中即算命中，并取最靠前的位置
        let (ci, cs) = rank_against_references(&["Grenades", "grenade"], &rows[0].references);
        assert_eq!((ci, cs), (Some(0), Some(1)));
        let (ci, cs) = rank_against_references(&["x"], &rows[0].references);
        assert_eq!((ci, cs), (None, None));

        let report = run_eval("tags", &rows);
        assert!(report.by_tag.contains_key("negative:clean"));
        assert!(report.by_tag.contains_key("untagged"));
        let rules = parse_gate_spec("tag:negative:clean.top1>=0", false).expect("tag gate");
        assert_eq!(rules[0].label, "tag:negative:clean.top1");
        assert!(evaluate_gates(&report, &rules, vec!["t".into()], false).passed);
        let missing = parse_gate_spec("tag:noise:space_shift.top1>=0", false).expect("tag gate");
        assert!(!evaluate_gates(&report, &missing, vec!["t".into()], false).passed);
        assert!(parse_gate_spec("tag:Bad.top1>=1", false).is_err());

        let json = render_json(&report, None, 0);
        assert!(json.contains("\"tags\": {"));
        assert!(render_text(&report, None, 0).contains("By notes tag:"));
    }

    #[test]
    fn gate_presets_custom_rules_and_missing_category() {
        let mut by_cat = BTreeMap::new();
        let mut exact = CategoryStats::default();
        exact.record(Some(0), Some(1), &keys(&[50])); // ci top1=100%, strict top1=0%
        let mut simple = CategoryStats::default();
        simple.record(Some(0), Some(0), &keys(&[60]));
        let mut phonetic = CategoryStats::default();
        phonetic.record(Some(2), Some(2), &keys(&[70]));
        let mut overall = CategoryStats::default();
        overall.record(Some(0), Some(1), &keys(&[50]));
        overall.record(Some(0), Some(0), &keys(&[60]));
        overall.record(Some(2), Some(2), &keys(&[70]));
        overall.finalize();

        by_cat.insert(Category::Exact, exact);
        by_cat.insert(Category::TypoSimple, simple);
        by_cat.insert(Category::TypoPhonetic, phonetic);

        let report = EvalReport {
            dataset: "mock".to_string(),
            by_cat,
            overall,
            failures: Vec::new(),
            l3_stats: None,
            by_tag: BTreeMap::new(),
        };

        let p1_rules = parse_gate_spec("phase1", false).expect("phase1");
        let p1 = evaluate_gates(&report, &p1_rules, vec!["phase1".into()], false);
        assert!(p1.passed);

        let p1_strict_rules = parse_gate_spec("phase1", true).expect("phase1 strict");
        let p1_strict = evaluate_gates(&report, &p1_strict_rules, vec!["phase1".into()], true);
        assert!(!p1_strict.passed);

        // phase2 需要 context_realword，但 mock 中缺失该类，必须判定失败而非静默放行
        let p2_rules = parse_gate_spec("phase2", false).expect("phase2");
        let p2 = evaluate_gates(&report, &p2_rules, vec!["phase2".into()], false);
        assert!(!p2.passed);
        assert!(p2.checks.iter().any(|c| !c.passed && c.actual.is_none()));

        let custom = parse_gate_spec("exact.top1>=99.5", false).expect("custom");
        assert!(evaluate_gates(&report, &custom, vec!["custom".into()], false).passed);
        assert!(parse_gate_spec("unknown.top1>=50", false).is_err());
    }

    #[test]
    fn json_and_markdown_reports_escape_special_chars() {
        let rows = parse_tsv("context_realword\ta|b \"q\"\tform\\x\tfrom\tpipe & quote\n")
            .expect("parse");
        let report = run_eval("test\"ds", &rows);
        let rules = parse_gate_spec("phase0", false).expect("phase0");
        let gate = evaluate_gates(&report, &rules, vec!["phase0".into()], false);

        let text = render_text(&report, Some(&gate), 5);
        assert!(text.contains("strict1%"));
        assert!(text.contains("Gate Result:"));

        let md = render_markdown(&report, Some(&gate), 5);
        assert!(md.contains("| category | total |"));
        assert!(md.contains("a\\|b"));

        let json = render_json(&report, Some(&gate), 5);
        assert!(json.contains("\"dataset\": \"test\\\"ds\""));
        assert!(json.contains("\"strict_top1_pct\""));
        assert!(json.contains("\"gate\": {"));
        assert!(json.contains("\\\\x"));
    }

    #[test]
    fn cli_parses_flags_and_rejects_bad_args() {
        let args = parse_args([
            "eval/benchmarks/core_v1.tsv",
            "--format",
            "json",
            "--json-out",
            "/tmp/out.json",
            "--md-out",
            "/tmp/out.md",
            "--gate",
            "phase1",
            "--gate",
            "sentence.top3>=80",
            "--strict-gate",
            "--show-failures",
            "12",
            "--lexicon",
            "data/compiled/lexicon.animelex.bin",
            "--ngram",
            "data/compiled/ngram.animengr.bin",
            "--model",
            "data/compiled/models/SmolLM2-135M-Q4_K_M.gguf",
            "--gpu-layers",
            "0",
            "--rerank-weight",
            "2.0",
            "--max-samples",
            "50",
        ])
        .expect("parse valid cli");

        assert_eq!(args.tsv_path, "eval/benchmarks/core_v1.tsv");
        assert_eq!(args.format, OutputFormat::Json);
        assert_eq!(args.json_out.as_deref(), Some("/tmp/out.json"));
        assert_eq!(args.md_out.as_deref(), Some("/tmp/out.md"));
        assert_eq!(args.gates, vec!["phase1", "sentence.top3>=80"]);
        assert!(args.strict_gate);
        assert_eq!(args.show_failures, 12);
        assert_eq!(
            args.lexicon_path.as_deref(),
            Some("data/compiled/lexicon.animelex.bin")
        );
        assert_eq!(
            args.ngram_path.as_deref(),
            Some("data/compiled/ngram.animengr.bin")
        );
        assert_eq!(
            args.model_path.as_deref(),
            Some("data/compiled/models/SmolLM2-135M-Q4_K_M.gguf")
        );
        assert_eq!(args.gpu_layers, 0);
        assert_eq!(args.rerank_weight, Some(2.0));
        assert_eq!(args.max_samples, Some(50));

        assert!(parse_args(["--format", "xml"]).is_err());
        assert!(parse_args(["--gpu-layers", "abc"]).is_err());
        assert!(parse_args(["a.tsv", "b.tsv"]).is_err());
    }
}
