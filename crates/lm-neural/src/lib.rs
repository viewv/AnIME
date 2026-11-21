//! L3 端侧神经语言模型异步精排与续写接口定义（Phase 2 接入实际模型）。

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_neural_reports_backend_and_empty_results() {
        let model = NoopNeuralModel;
        assert_eq!(model.backend_name(), "noop");
        assert!(model.rerank_sentences("", "test", &[]).is_empty());
        assert!(model.predict_continuations("Hello ", "w", 3).is_empty());
    }
}
