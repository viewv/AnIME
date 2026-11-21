//! 评测报告格式化器：支持文本表格、Markdown（GFM）与零依赖 RFC 8259 JSON 导出。

use std::fmt::Write as _;

use crate::gate::GateSummary;
use crate::metrics::{CategoryStats, EvalReport};

pub fn render_text(report: &EvalReport, gate: Option<&GateSummary>, max_failures: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "AnIME Offline Eval Report ({})", report.dataset);
    let _ = writeln!(
        out,
        "{:<18} {:>5} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8} {:>9} {:>9}",
        "category",
        "total",
        "top1%",
        "top3%",
        "top5%",
        "strict1%",
        "strict3%",
        "strict5%",
        "p50(us)",
        "p99(us)"
    );
    let _ = writeln!(out, "{}", "-".repeat(95));

    for (cat, stats) in &report.by_cat {
        write_text_row(&mut out, cat.as_str(), stats);
    }
    let _ = writeln!(out, "{}", "-".repeat(95));
    write_text_row(&mut out, "TOTAL", &report.overall);

    if let Some(g) = gate {
        let _ = writeln!(
            out,
            "\nGate Result: {} (specs: {}, strict_mode: {})",
            if g.passed { "PASS" } else { "FAIL" },
            g.specs.join(", "),
            g.strict_mode
        );
        for check in &g.checks {
            let status = if check.passed { "PASS" } else { "FAIL" };
            let unit = check.rule.metric.unit();
            match check.actual {
                Some(val) => {
                    let _ = writeln!(
                        out,
                        "  [{status}] {:<28} actual={val:.1}{unit} (expected {} {:.1}{unit})",
                        check.rule.label,
                        check.rule.op.as_str(),
                        check.rule.threshold
                    );
                }
                None => {
                    let note = check.note.as_deref().unwrap_or("missing");
                    let _ = writeln!(
                        out,
                        "  [{status}] {:<28} actual=N/A ({note}; expected {} {:.1}{unit})",
                        check.rule.label,
                        check.rule.op.as_str(),
                        check.rule.threshold
                    );
                }
            }
        }
    }

    if max_failures > 0 && !report.failures.is_empty() {
        let shown = report.failures.len().min(max_failures);
        let _ = writeln!(
            out,
            "\nSample Misses (showing {shown} of {}):",
            report.failures.len()
        );
        for fail in report.failures.iter().take(shown) {
            let top1 = fail.top1_actual.as_deref().unwrap_or("<none>");
            let _ = writeln!(
                out,
                "  [{}] ctx={:?} input={:?} expected={:?} top1={:?} ci_rank={:?} cs_rank={:?}",
                fail.category,
                fail.context,
                fail.input,
                fail.expected,
                top1,
                fail.ci_rank,
                fail.cs_rank
            );
        }
    }

    out
}

fn write_text_row(out: &mut String, label: &str, stats: &CategoryStats) {
    let _ = writeln!(
        out,
        "{:<18} {:>5} {:>6.1}% {:>6.1}% {:>6.1}% {:>7.1}% {:>7.1}% {:>7.1}% {:>9} {:>9}",
        label,
        stats.total,
        stats.top1_pct(),
        stats.top3_pct(),
        stats.top5_pct(),
        stats.strict_top1_pct(),
        stats.strict_top3_pct(),
        stats.strict_top5_pct(),
        stats.p50_us(),
        stats.p99_us()
    );
}

pub fn render_markdown(
    report: &EvalReport,
    gate: Option<&GateSummary>,
    max_failures: usize,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# AnIME Offline Eval Report\n");
    let _ = writeln!(
        out,
        "- Dataset: `{}`\n- Total samples: `{}`\n- Overall Top-1 (case-insensitive / strict): `{:.1}%` / `{:.1}%`\n- Overall Latency (p50 / p99): `{} us` / `{} us`\n",
        report.dataset,
        report.overall.total,
        report.overall.top1_pct(),
        report.overall.strict_top1_pct(),
        report.overall.p50_us(),
        report.overall.p99_us()
    );

    let _ = writeln!(out, "## Category Metrics\n");
    let _ = writeln!(
        out,
        "| category | total | top1% | top3% | top5% | strict1% | strict3% | strict5% | p50(us) | p99(us) |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for (cat, stats) in &report.by_cat {
        write_md_row(&mut out, cat.as_str(), stats);
    }
    write_md_row(&mut out, "**TOTAL**", &report.overall);

    if let Some(g) = gate {
        let _ = writeln!(
            out,
            "\n## Gate Checks ({})\n",
            if g.passed { "PASS" } else { "FAIL" }
        );
        let _ = writeln!(out, "| rule | actual | threshold | status |");
        let _ = writeln!(out, "|---|---:|---:|---|");
        for check in &g.checks {
            let unit = check.rule.metric.unit();
            let actual_str = match check.actual {
                Some(v) => format!("{v:.1}{unit}"),
                None => format!("N/A ({})", check.note.as_deref().unwrap_or("missing")),
            };
            let _ = writeln!(
                out,
                "| `{}` | {} | {} {:.1}{unit} | {} |",
                check.rule.label,
                actual_str,
                check.rule.op.as_str(),
                check.rule.threshold,
                if check.passed { "PASS" } else { "FAIL" }
            );
        }
    }

    if max_failures > 0 && !report.failures.is_empty() {
        let shown = report.failures.len().min(max_failures);
        let _ = writeln!(
            out,
            "\n## Sample Misses (showing {shown} of {})\n",
            report.failures.len()
        );
        let _ = writeln!(
            out,
            "| category | context | input | expected | top1 | ci_rank | cs_rank |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---:|---:|");
        for fail in report.failures.iter().take(shown) {
            let top1 = fail.top1_actual.as_deref().unwrap_or("<none>");
            let _ = writeln!(
                out,
                "| `{}` | `{}` | `{}` | `{}` | `{}` | {:?} | {:?} |",
                fail.category,
                md_escape(&fail.context),
                md_escape(&fail.input),
                md_escape(&fail.expected),
                md_escape(top1),
                fail.ci_rank,
                fail.cs_rank
            );
        }
    }

    out
}

fn write_md_row(out: &mut String, label: &str, stats: &CategoryStats) {
    let _ = writeln!(
        out,
        "| {} | {} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {} | {} |",
        label,
        stats.total,
        stats.top1_pct(),
        stats.top3_pct(),
        stats.top5_pct(),
        stats.strict_top1_pct(),
        stats.strict_top3_pct(),
        stats.strict_top5_pct(),
        stats.p50_us(),
        stats.p99_us()
    );
}

fn md_escape(raw: &str) -> String {
    raw.replace('|', "\\|")
        .replace('\n', " ")
        .replace('\r', " ")
}

pub fn render_json(report: &EvalReport, gate: Option<&GateSummary>, max_failures: usize) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    let _ = writeln!(out, "  \"dataset\": {},", json_str(&report.dataset));
    out.push_str("  \"overall\": ");
    write_json_stats(&mut out, &report.overall, 2);
    out.push_str(",\n");

    out.push_str("  \"categories\": {\n");
    let cat_count = report.by_cat.len();
    for (idx, (cat, stats)) in report.by_cat.iter().enumerate() {
        let _ = write!(out, "    {}: ", json_str(cat.as_str()));
        write_json_stats(&mut out, stats, 4);
        if idx + 1 < cat_count {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  },\n");

    match gate {
        Some(g) => {
            out.push_str("  \"gate\": {\n");
            let specs_json: Vec<String> = g.specs.iter().map(|s| json_str(s)).collect();
            let _ = writeln!(out, "    \"specs\": [{}],", specs_json.join(", "));
            let _ = writeln!(out, "    \"strict_mode\": {},", g.strict_mode);
            let _ = writeln!(out, "    \"passed\": {},", g.passed);
            out.push_str("    \"checks\": [\n");
            let check_count = g.checks.len();
            for (idx, check) in g.checks.iter().enumerate() {
                let actual_json = match check.actual {
                    Some(v) if v.is_finite() => format!("{v:.4}"),
                    _ => "null".to_string(),
                };
                let note_json = match &check.note {
                    Some(n) => json_str(n),
                    None => "null".to_string(),
                };
                let _ = write!(
                    out,
                    "      {{\"rule\": {}, \"target\": {}, \"metric\": {}, \"op\": {}, \"threshold\": {:.4}, \"actual\": {actual_json}, \"passed\": {}, \"note\": {note_json}}}",
                    json_str(&check.rule.label),
                    json_str(check.rule.target.as_str()),
                    json_str(check.rule.metric.as_str()),
                    json_str(check.rule.op.as_str()),
                    check.rule.threshold,
                    check.passed
                );
                if idx + 1 < check_count {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str("    ]\n");
            out.push_str("  },\n");
        }
        None => {
            out.push_str("  \"gate\": null,\n");
        }
    }

    let _ = writeln!(out, "  \"failures_total\": {},", report.failures.len());
    let shown = if max_failures == 0 {
        0
    } else {
        report.failures.len().min(max_failures)
    };
    out.push_str("  \"failures\": [\n");
    for (idx, fail) in report.failures.iter().take(shown).enumerate() {
        let top1_json = match &fail.top1_actual {
            Some(s) => json_str(s),
            None => "null".to_string(),
        };
        let ci_json = match fail.ci_rank {
            Some(r) => r.to_string(),
            None => "null".to_string(),
        };
        let cs_json = match fail.cs_rank {
            Some(r) => r.to_string(),
            None => "null".to_string(),
        };
        let _ = write!(
            out,
            "    {{\"category\": {}, \"context\": {}, \"input\": {}, \"expected\": {}, \"top1_actual\": {top1_json}, \"ci_rank\": {ci_json}, \"cs_rank\": {cs_json}}}",
            json_str(fail.category.as_str()),
            json_str(&fail.context),
            json_str(&fail.input),
            json_str(&fail.expected)
        );
        if idx + 1 < shown {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

fn write_json_stats(out: &mut String, stats: &CategoryStats, indent: usize) {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 2);
    out.push_str("{\n");
    let _ = writeln!(out, "{inner}\"total\": {},", stats.total);
    let _ = writeln!(out, "{inner}\"top1_hits\": {},", stats.top1);
    let _ = writeln!(out, "{inner}\"top3_hits\": {},", stats.top3);
    let _ = writeln!(out, "{inner}\"top5_hits\": {},", stats.top5);
    let _ = writeln!(out, "{inner}\"top1_pct\": {:.2},", stats.top1_pct());
    let _ = writeln!(out, "{inner}\"top3_pct\": {:.2},", stats.top3_pct());
    let _ = writeln!(out, "{inner}\"top5_pct\": {:.2},", stats.top5_pct());
    let _ = writeln!(out, "{inner}\"strict_top1_hits\": {},", stats.strict_top1);
    let _ = writeln!(out, "{inner}\"strict_top3_hits\": {},", stats.strict_top3);
    let _ = writeln!(out, "{inner}\"strict_top5_hits\": {},", stats.strict_top5);
    let _ = writeln!(
        out,
        "{inner}\"strict_top1_pct\": {:.2},",
        stats.strict_top1_pct()
    );
    let _ = writeln!(
        out,
        "{inner}\"strict_top3_pct\": {:.2},",
        stats.strict_top3_pct()
    );
    let _ = writeln!(
        out,
        "{inner}\"strict_top5_pct\": {:.2},",
        stats.strict_top5_pct()
    );
    let _ = writeln!(out, "{inner}\"latency_p50_us\": {},", stats.p50_us());
    let _ = writeln!(out, "{inner}\"latency_p99_us\": {}", stats.p99_us());
    let _ = write!(out, "{pad}}}");
}

fn json_str(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
