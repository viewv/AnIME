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

#[derive(Debug, Clone, PartialEq)]
pub struct CliArgs {
    pub tsv_path: String,
    pub format: OutputFormat,
    pub json_out: Option<String>,
    pub md_out: Option<String>,
    pub gates: Vec<String>,
    pub strict_gate: bool,
    pub show_failures: usize,
    pub group_by_tag: bool,
    pub interactive: bool,
    pub lexicon_path: Option<String>,
    pub ngram_path: Option<String>,
    pub model_path: Option<String>,
    pub gpu_layers: u32,
    pub rerank_weight: Option<f32>,
    pub max_samples: Option<usize>,
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
            group_by_tag: false,
            interactive: false,
            lexicon_path: None,
            ngram_path: None,
            model_path: None,
            gpu_layers: 999,
            rerank_weight: None,
            max_samples: None,
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
  --gate <PRESET|RULE>      Gate rule (phase0..phase3 or <target>.<metric>>=|<=>val; repeatable)
  --strict-gate             Use strict case-sensitive accuracy metrics for gate rules
  --interactive             Also report per-keystroke interaction metrics (keys saved, top-1 dropouts, select index, CI95)
  --group-by-tag            Also report metrics per notes tag (e.g. noise:leading_drop, negative:clean)
  --show-failures <N>       Include up to N sample misses in output (default: 0)
  --lexicon <PATH>          Load compiled .animelex.bin lexicon asset
  --ngram <PATH>            Load compiled .animengr.bin n-gram asset
  --model <PATH>            Load L3 GGUF model for neural reranking benchmark
  --gpu-layers <N>          GPU layers for L3 offload (default: 999; 0 = force CPU)
  --rerank-weight <F32>     Override L3 rerank fusion weight (default: library default)
  --max-samples <N>         Evaluate at most N rows from TSV
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
            "--group-by-tag" => cfg.group_by_tag = true,
            "--interactive" => cfg.interactive = true,
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
            "--lexicon" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--lexicon requires a path".to_string())?;
                cfg.lexicon_path = Some(val);
            }
            "--ngram" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--ngram requires a path".to_string())?;
                cfg.ngram_path = Some(val);
            }
            "--model" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--model requires a path".to_string())?;
                cfg.model_path = Some(val);
            }
            "--gpu-layers" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--gpu-layers requires a number".to_string())?;
                cfg.gpu_layers = val
                    .parse::<u32>()
                    .map_err(|_| format!("invalid --gpu-layers integer '{val}'"))?;
            }
            "--rerank-weight" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--rerank-weight requires a float".to_string())?;
                cfg.rerank_weight = Some(
                    val.parse::<f32>()
                        .map_err(|_| format!("invalid --rerank-weight float '{val}'"))?,
                );
            }
            "--max-samples" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--max-samples requires a number".to_string())?;
                cfg.max_samples = Some(
                    val.parse::<usize>()
                        .map_err(|_| format!("invalid --max-samples integer '{val}'"))?,
                );
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
