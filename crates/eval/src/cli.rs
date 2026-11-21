//! CLI 参数解析器，零外部依赖支持报告导出与门槛配置。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    Markdown,
}

impl OutputFormat {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            "md" | "markdown" => Ok(Self::Markdown),
            other => Err(format!(
                "unknown --format '{other}' (expected text, json, or md)"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub tsv_path: String,
    pub format: OutputFormat,
    pub json_out: Option<String>,
    pub md_out: Option<String>,
    pub gates: Vec<String>,
    pub strict_gate: bool,
    pub show_failures: usize,
    pub help: bool,
}

impl Default for CliArgs {
    fn default() -> Self {
        Self {
            tsv_path: "eval/fixtures/smoke.tsv".to_string(),
            format: OutputFormat::Text,
            json_out: None,
            md_out: None,
            gates: Vec::new(),
            strict_gate: false,
            show_failures: 0,
            help: false,
        }
    }
}

pub fn usage_text() -> &'static str {
    "Usage: anime-eval [TSV_PATH] [OPTIONS]

Options:
  --format <text|json|md>   Stdout output format (default: text)
  --json-out <PATH>         Write JSON report to PATH
  --md-out <PATH>           Write Markdown report to PATH
  --gate <PRESET|RULE>      Gate rule (phase0|phase1|phase2 or <target>.<metric>>=|<=>val; repeatable)
  --strict-gate             Use strict case-sensitive accuracy metrics for gate rules
  --show-failures <N>       Include up to N sample misses in output (default: 0)
  -h, --help                Show this help message"
}

pub fn parse_args<I, S>(args: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut cfg = CliArgs::default();
    let mut seen_path = false;
    let mut iter = args.into_iter().map(|s| s.as_ref().to_string());

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                cfg.help = true;
                return Ok(cfg);
            }
            "--strict-gate" => cfg.strict_gate = true,
            "--format" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--format requires a value".to_string())?;
                cfg.format = OutputFormat::parse(&val)?;
            }
            "--json-out" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--json-out requires a path".to_string())?;
                cfg.json_out = Some(val);
            }
            "--md-out" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--md-out requires a path".to_string())?;
                cfg.md_out = Some(val);
            }
            "--gate" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--gate requires a preset or rule".to_string())?;
                cfg.gates.push(val);
            }
            "--show-failures" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--show-failures requires a number".to_string())?;
                cfg.show_failures = val
                    .parse::<usize>()
                    .map_err(|_| format!("invalid --show-failures integer '{val}'"))?;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'"));
            }
            positional => {
                if seen_path {
                    return Err(format!("unexpected extra positional argument '{positional}'"));
                }
                cfg.tsv_path = positional.to_string();
                seen_path = true;
            }
        }
    }

    Ok(cfg)
}
