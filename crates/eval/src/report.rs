//! 评测报告格式化器：支持文本表格、Markdown（GFM）与零依赖 RFC 8259 JSON 导出。

use std::fmt::Write as _;

use crate::gate::GateSummary;
use crate::metrics::{CategoryStats, EvalReport};

pub fn render_text(report: &EvalReport, gate: Option<&GateSummary>, max_failures: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "AnIME Offline Eval Report ({})", report.dataset);
    let _ = writeln!(
        out,
        "{:<18} {:>5} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8} {:>9} {:>9} {:>10} {:>11}",
        "category",
        "total",
        "top1%",
        "top3%",
        "top5%",
        "strict1%",
        "strict3%",
        "strict5%",
        "key50(us)",
        "key99(us)",
        "keymax(us)",
        "sample50(us)"
    );
    let _ = writeln!(out, "{}", "-".repeat(118));

    for (cat, stats) in &report.by_cat {
        write_text_row(&mut out, cat.as_str(), stats);
    }
    let _ = writeln!(out, "{}", "-".repeat(118));
    write_text_row(&mut out, "TOTAL", &report.overall);

    if !report.by_tag.is_empty() {
        let _ = writeln!(out, "\nBy notes tag:");
        for (tag, stats) in &report.by_tag {
            write_text_row(&mut out, tag, stats);
        }
    }

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

    if let Some(ref l3) = report.l3_stats {
        let _ = writeln!(
            out,
            "\nL3 Neural Summary: backend={}, model={}, gpu_offload={}, device={}, warmup={}ms, calls={}, p50={}us, p99={}us, mean={}us",
            l3.backend,
            l3.model_name,
            l3.gpu_offload_active,
            l3.device_summary,
            l3.warmup_ms,
            l3.calls,
            l3.p50_us,
            l3.p99_us,
            l3.mean_us
        );
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
        "{:<18} {:>5} {:>6.1}% {:>6.1}% {:>6.1}% {:>7.1}% {:>7.1}% {:>7.1}% {:>9} {:>9} {:>10} {:>11}",
        label,
        stats.total,
        stats.top1_pct(),
        stats.top3_pct(),
        stats.top5_pct(),
        stats.strict_top1_pct(),
        stats.strict_top3_pct(),
        stats.strict_top5_pct(),
        stats.key_p50_us(),
        stats.key_p99_us(),
        stats.key_max_us(),
        stats.sample_p50_us()
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
        "- Dataset: `{}`\n- Total samples: `{}`\n- Overall Top-1 (case-insensitive / strict): `{:.1}%` / `{:.1}%`\n- Overall Key Latency (p50 / p99 / max): `{} us` / `{} us` / `{} us`\n- Sample Total Latency p50 (reference only): `{} us`\n",
        report.dataset,
        report.overall.total,
        report.overall.top1_pct(),
        report.overall.strict_top1_pct(),
        report.overall.key_p50_us(),
        report.overall.key_p99_us(),
        report.overall.key_max_us(),
        report.overall.sample_p50_us()
    );

    let _ = writeln!(out, "## Category Metrics\n");
    let _ = writeln!(
        out,
        "| category | total | top1% | top3% | top5% | strict1% | strict3% | strict5% | key50(us) | key99(us) | keymax(us) | sample50(us) |"
    );
    let _ = writeln!(
        out,
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    );
    for (cat, stats) in &report.by_cat {
        write_md_row(&mut out, cat.as_str(), stats);
    }
    write_md_row(&mut out, "**TOTAL**", &report.overall);

    if !report.by_tag.is_empty() {
        let _ = writeln!(out, "\n## Tag Metrics\n");
        let _ = writeln!(
            out,
            "| tag | total | top1% | top3% | top5% | strict1% | strict3% | strict5% | key50(us) | key99(us) | keymax(us) | sample50(us) |"
        );
        let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
        for (tag, stats) in &report.by_tag {
            write_md_row(&mut out, &md_escape(tag), stats);
        }
    }

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

    if let Some(ref l3) = report.l3_stats {
        let _ = writeln!(out, "\n## L3 Neural Summary\n");
        let _ = writeln!(
            out,
            "- Backend: `{}` (`gpu_offload`: `{}`, device: `{}`)\n- Model: `{}` (`warmup`: `{} ms`)\n- Inference calls: `{}` (`p50`: `{} us`, `p99`: `{} us`, `mean`: `{} us`)",
            l3.backend,
            l3.gpu_offload_active,
            md_escape(&l3.device_summary),
            md_escape(&l3.model_name),
            l3.warmup_ms,
            l3.calls,
            l3.p50_us,
            l3.p99_us,
            l3.mean_us
        );
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
        "| {} | {} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {} | {} | {} | {} |",
        label,
        stats.total,
        stats.top1_pct(),
        stats.top3_pct(),
        stats.top5_pct(),
        stats.strict_top1_pct(),
        stats.strict_top3_pct(),
        stats.strict_top5_pct(),
        stats.key_p50_us(),
        stats.key_p99_us(),
        stats.key_max_us(),
        stats.sample_p50_us()
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

    if !report.by_tag.is_empty() {
        out.push_str("  \"tags\": {\n");
        let tag_count = report.by_tag.len();
        for (idx, (tag, stats)) in report.by_tag.iter().enumerate() {
            let _ = write!(out, "    {}: ", json_str(tag));
            write_json_stats(&mut out, stats, 4);
            if idx + 1 < tag_count {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  },\n");
    }

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
                    json_str(&check.rule.target.label()),
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
    out.push_str("  ]");
    if let Some(ref l3) = report.l3_stats {
        out.push_str(",\n  \"l3\": {\n");
        let _ = writeln!(out, "    \"backend\": {},", json_str(&l3.backend));
        let _ = writeln!(out, "    \"model_name\": {},", json_str(&l3.model_name));
        let _ = writeln!(out, "    \"gpu_offload_active\": {},", l3.gpu_offload_active);
        let _ = writeln!(out, "    \"device_summary\": {},", json_str(&l3.device_summary));
        let _ = writeln!(out, "    \"warmup_ms\": {},", l3.warmup_ms);
        let _ = writeln!(out, "    \"calls\": {},", l3.calls);
        let _ = writeln!(out, "    \"p50_us\": {},", l3.p50_us);
        let _ = writeln!(out, "    \"p99_us\": {},", l3.p99_us);
        let _ = writeln!(out, "    \"mean_us\": {}", l3.mean_us);
        out.push_str("  }\n");
    } else {
        out.push('\n');
    }
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
    let _ = writeln!(out, "{inner}\"key_latency_p50_us\": {},", stats.key_p50_us());
    let _ = writeln!(out, "{inner}\"key_latency_p99_us\": {},", stats.key_p99_us());
    let _ = writeln!(out, "{inner}\"key_latency_max_us\": {},", stats.key_max_us());
    let _ = writeln!(out, "{inner}\"sample_total_p50_us\": {},", stats.sample_p50_us());
    let _ = writeln!(out, "{inner}\"sample_total_p99_us\": {}", stats.sample_p99_us());
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
