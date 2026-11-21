//! L2 词格（Word Lattice）Viterbi 解码与本机学习 Trait 定义。

#[derive(Debug, Clone, PartialEq)]
pub struct SlotCandidate {
    pub word: String,
    pub lex_score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LatticeSlot {
    pub raw_token: String,
    pub start_char: usize,
    pub end_char: usize,
    pub candidates: Vec<SlotCandidate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedSentence {
    pub text: String,
    pub score: f32,
    pub word_count: usize,
}

pub trait NgramModel: Send + Sync {
    fn score_word(&self, prev_word: Option<&str>, word: &str) -> f32;

    fn decode_lattice(
        &self,
        context_before: &str,
        slots: &[LatticeSlot],
        limit: usize,
    ) -> Vec<DecodedSentence>;

    fn learn_commit(&self, context_before: &str, committed_text: &str, privacy: bool);
}

/// Phase 0 空实现桩：直接取每个词槽的最高分候选或原词拼句，不持久化学习状态。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopNgramModel;

impl NgramModel for NoopNgramModel {
    fn score_word(&self, _prev_word: Option<&str>, _word: &str) -> f32 {
        0.0
    }

    fn decode_lattice(
        &self,
        _context_before: &str,
        slots: &[LatticeSlot],
        limit: usize,
    ) -> Vec<DecodedSentence> {
        if slots.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut words = Vec::with_capacity(slots.len());
        let mut total_score = 0.0_f32;
        for slot in slots {
            if let Some(best) = slot.candidates.first() {
                words.push(best.word.as_str());
                total_score += best.lex_score;
            } else {
                words.push(slot.raw_token.as_str());
            }
        }
        vec![DecodedSentence {
            text: words.join(" "),
            score: total_score,
            word_count: slots.len(),
        }]
    }

    fn learn_commit(&self, _context_before: &str, _committed_text: &str, _privacy: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_ngram_joins_top_slot_candidates() {
        let model = NoopNgramModel;
        let slots = vec![
            LatticeSlot {
                raw_token: "i".into(),
                start_char: 0,
                end_char: 1,
                candidates: vec![SlotCandidate {
                    word: "I".into(),
                    lex_score: -1.0,
                }],
            },
            LatticeSlot {
                raw_token: "eat".into(),
                start_char: 2,
                end_char: 5,
                candidates: vec![SlotCandidate {
                    word: "eat".into(),
                    lex_score: -2.0,
                }],
            },
        ];
        let out = model.decode_lattice("", &slots, 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "I eat");
        assert_eq!(out[0].word_count, 2);
    }
}
