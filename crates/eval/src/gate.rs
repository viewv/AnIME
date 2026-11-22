//! 离线评测门槛检查（--gate）：内置 phase0~phase3 契约门禁并支持自定义规则。
//! 延迟门禁一律按单键口径（`eval-schema.md` §3）；单样本累计耗时只出现在报告中，不可作门禁。

use crate::metrics::{CategoryStats, EvalReport};
use crate::tsv::Category;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateTarget {
    Overall,
    Category(Category),
}

impl GateTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Overall => "total",
            Self::Category(cat) => cat.as_str(),
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "total" | "overall" => Some(Self::Overall),
            other => Category::parse(other).map(Self::Category),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateMetric {
    Top1,
    Top3,
    Top5,
    StrictTop1,
    StrictTop3,
    StrictTop5,
    KeyP50Us,
    KeyP99Us,
    KeyMaxUs,
}

impl GateMetric {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Top1 => "top1",
            Self::Top3 => "top3",
            Self::Top5 => "top5",
            Self::StrictTop1 => "strict_top1",
            Self::StrictTop3 => "strict_top3",
            Self::StrictTop5 => "strict_top5",
            Self::KeyP50Us => "key_p50_us",
            Self::KeyP99Us => "key_p99_us",
            Self::KeyMaxUs => "key_max_us",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Self::KeyP50Us | Self::KeyP99Us | Self::KeyMaxUs => "us",
            _ => "%",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "top1" => Some(Self::Top1),
            "top3" => Some(Self::Top3),
            "top5" => Some(Self::Top5),
            "strict_top1" => Some(Self::StrictTop1),
            "strict_top3" => Some(Self::StrictTop3),
            "strict_top5" => Some(Self::StrictTop5),
            // `p50_us` / `p99_us` 为旧名兼容别名，语义已统一为单键口径
            "key_p50_us" | "p50_us" => Some(Self::KeyP50Us),
            "key_p99_us" | "p99_us" => Some(Self::KeyP99Us),
            "key_max_us" => Some(Self::KeyMaxUs),
            _ => None,
        }
    }

    fn to_strict(self) -> Self {
        match self {
            Self::Top1 => Self::StrictTop1,
            Self::Top3 => Self::StrictTop3,
            Self::Top5 => Self::StrictTop5,
            other => other,
        }
    }

    fn extract(self, stats: &CategoryStats) -> f64 {
        match self {
            Self::Top1 => stats.top1_pct(),
            Self::Top3 => stats.top3_pct(),
            Self::Top5 => stats.top5_pct(),
            Self::StrictTop1 => stats.strict_top1_pct(),
            Self::StrictTop3 => stats.strict_top3_pct(),
            Self::StrictTop5 => stats.strict_top5_pct(),
            Self::KeyP50Us => stats.key_p50_us() as f64,
            Self::KeyP99Us => stats.key_p99_us() as f64,
            Self::KeyMaxUs => stats.key_max_us() as f64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOp {
    Ge,
    Le,
}

impl GateOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ge => ">=",
            Self::Le => "<=",
        }
    }

    fn matches(self, actual: f64, threshold: f64) -> bool {
        match self {
            Self::Ge => actual >= threshold,
            Self::Le => actual <= threshold,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateRule {
    pub label: String,
    pub target: GateTarget,
    pub metric: GateMetric,
    pub op: GateOp,
    pub threshold: f64,
}

impl GateRule {
    fn new(target: GateTarget, metric: GateMetric, op: GateOp, threshold: f64) -> Self {
        let label = format!("{}.{}", target.as_str(), metric.as_str());
        Self {
            label,
            target,
            metric,
            op,
            threshold,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateCheckResult {
    pub rule: GateRule,
    pub actual: Option<f64>,
    pub passed: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateSummary {
    pub specs: Vec<String>,
    pub strict_mode: bool,
    pub passed: bool,
    pub checks: Vec<GateCheckResult>,
}

pub fn parse_gate_spec(spec: &str, strict_gate: bool) -> Result<Vec<GateRule>, String> {
    let trimmed = spec.trim();
    let accuracy_metric = |m: GateMetric| -> GateMetric {
        if strict_gate {
            m.to_strict()
        } else {
            m
        }
    };

    match trimmed {
        "phase0" => Ok(vec![GateRule::new(
            GateTarget::Overall,
            GateMetric::KeyP99Us,
            GateOp::Le,
            500.0,
        )]),
        "phase1" => Ok(vec![
            GateRule::new(
                GateTarget::Category(Category::Exact),
                accuracy_metric(GateMetric::Top1),
                GateOp::Ge,
                98.0,
            ),
            GateRule::new(
                GateTarget::Category(Category::TypoSimple),
                accuracy_metric(GateMetric::Top1),
                GateOp::Ge,
                85.0,
            ),
            GateRule::new(
                GateTarget::Category(Category::TypoPhonetic),
                accuracy_metric(GateMetric::Top3),
                GateOp::Ge,
                75.0,
            ),
            GateRule::new(
                GateTarget::Overall,
                GateMetric::KeyP99Us,
                GateOp::Le,
                5000.0,
            ),
        ]),
        "phase2" => Ok(vec![
            GateRule::new(
                GateTarget::Category(Category::Exact),
                accuracy_metric(GateMetric::Top1),
                GateOp::Ge,
                99.0,
            ),
            GateRule::new(
                GateTarget::Category(Category::TypoSimple),
                accuracy_metric(GateMetric::Top1),
                GateOp::Ge,
                92.0,
            ),
            GateRule::new(
                GateTarget::Category(Category::TypoPhonetic),
                accuracy_metric(GateMetric::Top3),
                GateOp::Ge,
                85.0,
            ),
            GateRule::new(
                GateTarget::Category(Category::ContextRealword),
                accuracy_metric(GateMetric::Top3),
                GateOp::Ge,
                85.0,
            ),
            GateRule::new(
                GateTarget::Overall,
                GateMetric::KeyP99Us,
                GateOp::Le,
                5000.0,
            ),
        ]),
        "phase3" => {
            let mut rules: Vec<GateRule> = parse_gate_spec("phase2", strict_gate)?
                .into_iter()
                .filter(|r| r.metric.unit() != "us")
                .collect();
            rules.extend([
                GateRule::new(
                    GateTarget::Overall,
                    GateMetric::KeyP50Us,
                    GateOp::Le,
                    3000.0,
                ),
                GateRule::new(
                    GateTarget::Overall,
                    GateMetric::KeyP99Us,
                    GateOp::Le,
                    10_000.0,
                ),
                GateRule::new(
                    GateTarget::Overall,
                    GateMetric::KeyMaxUs,
                    GateOp::Le,
                    16_000.0,
                ),
            ]);
            Ok(rules)
        }
        _ => parse_custom_rule(trimmed, strict_gate).map(|r| vec![r]),
    }
}

fn parse_custom_rule(raw: &str, strict_gate: bool) -> Result<GateRule, String> {
    let (lhs, op, rhs) = if let Some((l, r)) = raw.split_once(">=") {
        (l, GateOp::Ge, r)
    } else if let Some((l, r)) = raw.split_once("<=") {
        (l, GateOp::Le, r)
    } else {
        return Err(format!(
            "invalid gate spec '{raw}': expected preset (phase0|phase1|phase2|phase3) or '<target>.<metric>>=<val>' / '<target>.<metric><=<val>'"
        ));
    };

    let (target_raw, metric_raw) = lhs.trim().split_once('.').ok_or_else(|| {
        format!("invalid gate left-hand side '{lhs}': expected '<target>.<metric>'")
    })?;

    let target = GateTarget::parse(target_raw)
        .ok_or_else(|| format!("unknown gate target '{target_raw}'"))?;
    let mut metric = GateMetric::parse(metric_raw)
        .ok_or_else(|| format!("unknown gate metric '{metric_raw}'"))?;
    if strict_gate {
        metric = metric.to_strict();
    }

    let threshold: f64 = rhs
        .trim()
        .parse()
        .map_err(|_| format!("invalid numeric gate threshold '{}'", rhs.trim()))?;
    if !threshold.is_finite() {
        return Err(format!("gate threshold must be finite, got '{rhs}'"));
    }

    Ok(GateRule::new(target, metric, op, threshold))
}

pub fn evaluate_gates(
    report: &EvalReport,
    rules: &[GateRule],
    specs: Vec<String>,
    strict_mode: bool,
) -> GateSummary {
    let mut checks = Vec::with_capacity(rules.len());
    let mut all_passed = true;

    for rule in rules {
        let stats = match rule.target {
            GateTarget::Overall => Some(&report.overall),
            GateTarget::Category(cat) => report.by_cat.get(&cat),
        };
        let result = match stats {
            Some(s) if s.total > 0 => {
                let actual = rule.metric.extract(s);
                let passed = rule.op.matches(actual, rule.threshold);
                if !passed {
                    all_passed = false;
                }
                GateCheckResult {
                    rule: rule.clone(),
                    actual: Some(actual),
                    passed,
                    note: None,
                }
            }
            _ => {
                all_passed = false;
                GateCheckResult {
                    rule: rule.clone(),
                    actual: None,
                    passed: false,
                    note: Some("no samples for target".to_string()),
                }
            }
        };
        checks.push(result);
    }

    GateSummary {
        specs,
        strict_mode,
        passed: all_passed,
        checks,
    }
}
