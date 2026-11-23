//! L3 端侧神经语言模型异步精排与续写接口及 llama.cpp GPU/CPU 推理后端。

mod config;
#[cfg(feature = "llama")]
mod llama_backend;
mod prompt;

use std::sync::Arc;

pub use config::{NeuralError, NeuralModelConfig, NeuralRuntimeInfo, PromptStyle, validate_gguf_header};
#[cfg(feature = "llama")]
pub use llama_backend::LlamaNeuralModel;
pub use prompt::{
    CORRECTION_END_MARK, build_correction_prompt, correction_target,
    ContinuationPlan, build_continuation_plan, extract_clean_continuation_word,
    normalize_rerank_scores, normalize_rerank_scores_top, split_prefix_and_candidates, trim_context_tail,
};

#[derive(Debug, Clone, PartialEq)]
pub struct NeuralCandidate {
    pub text: String,
    pub score: f32,
    pub is_continuation: bool,
}

pub trait NeuralSentenceModel: Send + Sync {
    fn backend_name(&self) -> &'static str;

    fn rerank_sentences(
        &self,
        context_before: &str,
        preedit: &str,
        candidates: &[String],
    ) -> Vec<NeuralCandidate>;

    fn predict_continuations(
        &self,
        context_before: &str,
        prefix: &str,
        limit: usize,
    ) -> Vec<NeuralCandidate>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopNeuralModel;

impl NeuralSentenceModel for NoopNeuralModel {
    fn backend_name(&self) -> &'static str {
        "noop"
    }

    fn rerank_sentences(
        &self,
        _context_before: &str,
        _preedit: &str,
        _candidates: &[String],
    ) -> Vec<NeuralCandidate> {
        Vec::new()
    }

    fn predict_continuations(
        &self,
        _context_before: &str,
        _prefix: &str,
        _limit: usize,
    ) -> Vec<NeuralCandidate> {
        Vec::new()
    }
}

pub fn load_neural_model(
    config: NeuralModelConfig,
) -> Result<(Arc<dyn NeuralSentenceModel>, NeuralRuntimeInfo), NeuralError> {
    #[cfg(feature = "llama")]
    {
        let model = LlamaNeuralModel::load(config)?;
        let info = model.runtime_info().clone();
        Ok((Arc::new(model), info))
    }
    #[cfg(not(feature = "llama"))]
    {
        validate_gguf_header(&config.model_path)?;
        Err(NeuralError::UnsupportedBackend(
            "anime-lm-neural compiled without 'llama' feature (enable '--features vulkan' or '--features llama')"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn noop_neural_reports_backend_and_empty_results() {
        let model = NoopNeuralModel;
        assert_eq!(model.backend_name(), "noop");
        assert!(model.rerank_sentences("", "test", &[]).is_empty());
        assert!(model.predict_continuations("Hello ", "w", 3).is_empty());
    }

    #[test]
    fn validate_gguf_rejects_bad_magic_and_missing_file() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let bad = std::env::temp_dir().join(format!("anime_bad_model_{nanos}.gguf"));
        fs::write(&bad, b"NOTGGUF123").expect("write temp");
        let err = validate_gguf_header(&bad).expect_err("must reject bad magic");
        assert!(matches!(err, NeuralError::InvalidFormat(_)));
        let _ = fs::remove_file(&bad);

        let missing = std::env::temp_dir().join(format!("anime_missing_{nanos}.gguf"));
        let err2 = validate_gguf_header(&missing).expect_err("must reject missing file");
        assert!(matches!(err2, NeuralError::Io(_)));
    }

    #[cfg(feature = "llama")]
    #[test]
    #[ignore = "requires local GGUF model asset downloaded by fetch_models.py"]
    fn llama_smollm2_135m_rerank_and_continuation_smoke() {
        let model_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/compiled/models/SmolLM2-135M-Q4_K_M.gguf");
        assert!(
            model_path.is_file(),
            "missing test GGUF '{}'",
            model_path.display()
        );

        let mut cfg = NeuralModelConfig::with_model_path(&model_path);
        cfg.n_gpu_layers = 0;
        let (model, info) = load_neural_model(cfg).expect("load SmolLM2-135M GGUF");
        assert_eq!(info.backend_name, "llama-cpu");
        assert!(!info.gpu_offload_active);

        // Zero-context single-word candidates should fast-path to empty.
        assert!(model
            .rerank_sentences("", "test", &["test".into(), "rest".into()])
            .is_empty());

        // Contextual real-word disambiguation: "Thank you for your " -> "form" vs "from".
        let reranked = model.rerank_sentences(
            "Thank you for your ",
            "form",
            &["from".into(), "form".into()],
        );
        assert_eq!(reranked.len(), 2);
        let s_form = reranked
            .iter()
            .find(|c| c.text == "form")
            .expect("form score")
            .score;
        let s_from = reranked
            .iter()
            .find(|c| c.text == "from")
            .expect("from score")
            .score;
        assert!(
            s_form > s_from,
            "expected 'form' ({s_form}) > 'from' ({s_from}) after 'Thank you for your '"
        );

        // Empty-context multi-word sentence reranking must still score using BOS/EOS seed.
        let sent_reranked = model.rerank_sentences(
            "",
            "I wanto to go home",
            &["I went to go home".into(), "I want to go home".into()],
        );
        assert_eq!(sent_reranked.len(), 2);
        assert!(sent_reranked.iter().all(|c| c.score.is_finite()));

        // Continuation prediction after trailing space.
        let conts = model.predict_continuations("Thank you ", "for ", 3);
        assert!(!conts.is_empty(), "expected non-empty continuations");
        assert!(
            conts.iter().all(|c| c.is_continuation && c.text.starts_with("for ")),
            "unexpected continuation items: {conts:?}"
        );
    }
}
