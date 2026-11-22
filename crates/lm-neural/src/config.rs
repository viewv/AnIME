use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// L3 端侧神经模型配置（平台无关，默认 GPU 全层卸载、不支持 GPU 时自动回退 CPU）。
#[derive(Debug, Clone, PartialEq)]
pub struct NeuralModelConfig {
    pub model_path: PathBuf,
    /// 请求卸载至 GPU（Vulkan / Metal / CUDA）的层数；999 表示全部层，`0` 表示强制纯 CPU。
    pub n_gpu_layers: u32,
    pub n_ctx: u32,
    pub n_threads: i32,
    pub max_context_tokens: usize,
    pub max_rerank_candidates: usize,
    pub max_continuation_tokens: usize,
    pub rerank_weight: f32,
    pub verbose_logs: bool,
}

impl NeuralModelConfig {
    pub fn with_model_path(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            n_gpu_layers: 999,
            n_ctx: 512,
            n_threads: 4,
            max_context_tokens: 48,
            max_rerank_candidates: 6,
            max_continuation_tokens: 3,
            rerank_weight: 1.6,
            verbose_logs: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeuralRuntimeInfo {
    pub backend_name: &'static str,
    pub model_name: String,
    pub gpu_layers_requested: u32,
    pub gpu_offload_active: bool,
    pub device_summary: String,
    pub warmup_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeuralError {
    Io(String),
    InvalidFormat(String),
    UnsupportedBackend(String),
    BackendInit(String),
    LoadModel(String),
    LoadContext(String),
}

impl fmt::Display for NeuralError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "neural model I/O error: {msg}"),
            Self::InvalidFormat(msg) => write!(f, "invalid GGUF model format: {msg}"),
            Self::UnsupportedBackend(msg) => write!(f, "neural backend unavailable: {msg}"),
            Self::BackendInit(msg) => write!(f, "neural backend init error: {msg}"),
            Self::LoadModel(msg) => write!(f, "failed to load neural model: {msg}"),
            Self::LoadContext(msg) => write!(f, "failed to create inference context: {msg}"),
        }
    }
}

impl std::error::Error for NeuralError {}

pub fn validate_gguf_header(path: &Path) -> Result<(), NeuralError> {
    let mut file = File::open(path)
        .map_err(|e| NeuralError::Io(format!("cannot open '{}': {e}", path.display())))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .map_err(|e| NeuralError::InvalidFormat(format!("truncated header '{}': {e}", path.display())))?;
    if &magic != b"GGUF" {
        return Err(NeuralError::InvalidFormat(format!(
            "expected GGUF magic header in '{}'",
            path.display()
        )));
    }
    Ok(())
}
