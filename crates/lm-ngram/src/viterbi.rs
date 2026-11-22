//! Top-K 二阶（3-gram）Beam Viterbi 词格整句解码器与上下文词史提取器。

use std::collections::{HashMap, HashSet};

use crate::DecodedSentence;
use crate::LatticeSlot;
use crate::format::SPECIAL_BOS;
use crate::model::NgramLanguageModel;
use crate::user_db::tokenize_words;

const BEAM_WIDTH: usize = 24;
const MAX_CANDS_PER_SLOT: usize = 10;
const PER_STATE_TOP_K: usize = 2;

// 发射与转移融合权重（原因注释）：
// 1. L1 对精确词带 EXACT_BONUS=6.5，因此单槽对 `slot_max` 做归一化（LEX_LAMBDA=0.55），
//    使语境 confusable（-1.8 相对先验）折算约 -0.99；
// 2. LM_LAMBDA=1.25 下，当上下文 2/3-gram 相对先验具备 ~+1.2 以上对数概率差（或前后双转移合计）时
//    能果断翻转真实语境错用（如 go form here -> go from here），而无强语境支撑的生僻真词保持零误改。
const LEX_LAMBDA: f32 = 0.55;
const LM_LAMBDA: f32 = 1.25;
const REL_LEX_FLOOR: f32 = -12.0;

#[derive(Clone)]
struct BeamState {
    score: f32,
    prev2_lower: Option<String>,
    prev1_lower: Option<String>,
    words: Vec<String>,
}

/// 从光标前文本提取供 2/3-gram 使用的最近两个小写词（遇句号/感叹号/问号/换行则重置到 `<bos>`）。
pub(crate) fn extract_context_history(context_before: &str) -> (Option<String>, Option<String>) {
    let trimmed_end = context_before.trim_end_matches([' ', '\t']);
    if trimmed_end.is_empty()
        || context_before.ends_with('\n')
        || context_before.ends_with('\r')
        || trimmed_end.ends_with('.')
        || trimmed_end.ends_with('!')
        || trimmed_end.ends_with('?')
    {
        return (None, Some(SPECIAL_BOS.to_string()));
    }

    let toks = tokenize_words(trimmed_end);
    match toks.len() {
        0 => (None, Some(SPECIAL_BOS.to_string())),
        1 => (
            Some(SPECIAL_BOS.to_string()),
            Some(toks[0].to_ascii_lowercase()),
        ),
        n => (
            Some(toks[n - 2].to_ascii_lowercase()),
            Some(toks[n - 1].to_ascii_lowercase()),
        ),
    }
}

pub(crate) fn decode_with_beam_viterbi(
    lm: &NgramLanguageModel,
    context_before: &str,
    slots: &[LatticeSlot],
    limit: usize,
) -> Vec<DecodedSentence> {
    if slots.is_empty() || limit == 0 {
        return Vec::new();
    }

    let (init_p2, init_p1) = extract_context_history(context_before);
    let mut beam = vec![BeamState {
        score: 0.0,
        prev2_lower: init_p2,
        prev1_lower: init_p1,
        words: Vec::with_capacity(slots.len()),
    }];

    let static_table = lm.static_table();
    let user_store = lm.user_store();

    for slot in slots {
        let fallback_word;
        let slot_cands: &[crate::SlotCandidate] = if slot.candidates.is_empty() {
            fallback_word = [crate::SlotCandidate {
                word: slot.raw_token.clone(),
                lex_score: -50.0,
            }];
            &fallback_word
        } else {
            let cap = slot.candidates.len().min(MAX_CANDS_PER_SLOT);
            &slot.candidates[..cap]
        };

        let slot_max_lex = slot_cands
            .iter()
            .map(|c| c.lex_score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut next_states = Vec::with_capacity(beam.len() * slot_cands.len());
        for state in &beam {
            for cand in slot_cands {
                let w_lower = cand.word.to_ascii_lowercase();
                let p1_ctx = state
                    .prev1_lower
                    .as_deref()
                    .filter(|&p| p != SPECIAL_BOS);

                let step_score = if let Some(table) = static_table {
                    let rel_lex = (cand.lex_score - slot_max_lex).max(REL_LEX_FLOOR);
                    let lm_logp = table.log10_trigram(
                        state.prev2_lower.as_deref(),
                        state.prev1_lower.as_deref(),
                        &w_lower,
                    );
                    let user_bonus = user_store.bigram_boost(p1_ctx, &w_lower);
                    LEX_LAMBDA * rel_lex + LM_LAMBDA * lm_logp + user_bonus
                } else {
                    cand.lex_score + user_store.bigram_boost(p1_ctx, &w_lower)
                };

                let mut words = state.words.clone();
                words.push(cand.word.clone());
                next_states.push(BeamState {
                    score: state.score + step_score,
                    prev2_lower: state.prev1_lower.clone(),
                    prev1_lower: Some(w_lower),
                    words,
                });
            }
        }

        next_states.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.words.cmp(&b.words))
        });

        let mut kept = Vec::with_capacity(BEAM_WIDTH);
        let mut state_counts: HashMap<(Option<String>, Option<String>), usize> =
            HashMap::with_capacity(BEAM_WIDTH);
        for st in next_states {
            let state_key = (st.prev2_lower.clone(), st.prev1_lower.clone());
            let cnt = state_counts.entry(state_key).or_insert(0);
            if *cnt >= PER_STATE_TOP_K {
                continue;
            }
            *cnt += 1;
            kept.push(st);
            if kept.len() >= BEAM_WIDTH {
                break;
            }
        }
        beam = kept;
    }

    let mut seen_texts = HashSet::with_capacity(limit * 2);
    let mut out = Vec::with_capacity(limit);
    let word_count = slots.len();

    for st in beam {
        let text = st.words.join(" ");
        if seen_texts.insert(text.to_ascii_lowercase()) {
            out.push(DecodedSentence {
                text,
                score: st.score,
                word_count,
            });
            if out.len() >= limit {
                break;
            }
        }
    }

    out
}
