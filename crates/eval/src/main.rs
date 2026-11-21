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

use cli::{OutputFormat, parse_args, usage_text};
use gate::{evaluate_gates, parse_gate_spec};
use metrics::run_eval;
use report::{render_json, render_markdown, render_text};
use tsv::parse_tsv;

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

    let rows = match parse_tsv(&content) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("invalid eval TSV '{}': {err}", args.tsv_path);
            process::exit(1);
        }
    };
    if rows.is_empty() {
        eprintln!("no eval cases found in '{}'", args.tsv_path);
        process::exit(1);
    }

    let report = run_eval(&args.tsv_path, &rows);
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
    use crate::metrics::{CategoryStats, EvalReport, run_eval, run_eval_with_session};
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

    #[test]
    fn gate_presets_custom_rules_and_missing_category() {
        let mut by_cat = BTreeMap::new();
        let mut exact = CategoryStats::default();
        exact.record(Some(0), Some(1), 50); // ci top1=100%, strict top1=0%
        let mut simple = CategoryStats::default();
        simple.record(Some(0), Some(0), 60);
        let mut phonetic = CategoryStats::default();
        phonetic.record(Some(2), Some(2), 70);
        let mut overall = CategoryStats::default();
        overall.record(Some(0), Some(1), 50);
        overall.record(Some(0), Some(0), 60);
        overall.record(Some(2), Some(2), 70);
        overall.finalize();

        by_cat.insert(Category::Exact, exact);
        by_cat.insert(Category::TypoSimple, simple);
        by_cat.insert(Category::TypoPhonetic, phonetic);

        let report = EvalReport {
            dataset: "mock".to_string(),
            by_cat,
            overall,
            failures: Vec::new(),
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
        ])
        .expect("parse valid cli");

        assert_eq!(args.tsv_path, "eval/benchmarks/core_v1.tsv");
        assert_eq!(args.format, OutputFormat::Json);
        assert_eq!(args.json_out.as_deref(), Some("/tmp/out.json"));
        assert_eq!(args.md_out.as_deref(), Some("/tmp/out.md"));
        assert_eq!(args.gates, vec!["phase1", "sentence.top3>=80"]);
        assert!(args.strict_gate);
        assert_eq!(args.show_failures, 12);

        assert!(parse_args(["--format", "xml"]).is_err());
        assert!(parse_args(["a.tsv", "b.tsv"]).is_err());
    }
}
