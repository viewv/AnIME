use crate::NeuralCandidate;

const MAX_CONTEXT_CHARS: usize = 240;

/// 在词边界处截取末尾上下文，避免从半个单词中间开始送给 BPE Tokenizer。
pub fn trim_context_tail(context_before: &str, max_chars: usize) -> &str {
    let total = context_before.chars().count();
    if total <= max_chars || max_chars == 0 {
        return context_before;
    }
    let skip = total - max_chars;
    let Some((start_byte, _)) = context_before.char_indices().nth(skip) else {
        return context_before;
    };
    let tail = &context_before[start_byte..];
    if let Some(ws_off) = tail.find(|c: char| c.is_ascii_whitespace()) {
        let after = &tail[ws_off..];
        if !after.trim().is_empty() {
            return after.trim_start_matches([' ', '\t']);
        }
    }
    tail
}

/// 将 `context_before` 与候选列表规整为 BPE 友好的 `(shared_prefix, candidate_suffixes)`：
/// 英文 BPE 分词器习惯将词前空格并入词 token（如 `Ġapple`），若保留前缀末尾空格再紧接无空格
/// 候选词会导致前缀末空格单独成 token，显著扭曲对数概率。
pub fn split_prefix_and_candidates(
    context_before: &str,
    candidates: &[String],
) -> (String, Vec<String>) {
    let tail = trim_context_tail(context_before, MAX_CONTEXT_CHARS);
    let has_trailing_space = tail.ends_with(' ') || tail.ends_with('\t');
    let trimmed_prefix = tail.trim_end_matches([' ', '\t']).to_string();
    let needs_leading_space =
        has_trailing_space && !trimmed_prefix.is_empty() && !trimmed_prefix.ends_with(['\n', '\r']);

    let formatted = candidates
        .iter()
        .map(|cand| {
            let core = cand.trim();
            if core.is_empty() {
                String::new()
            } else if needs_leading_space {
                format!(" {core}")
            } else {
                core.to_string()
            }
        })
        .collect();

    (trimmed_prefix, formatted)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationPlan {
    pub prompt: String,
    pub expect_leading_space: bool,
    pub join_base: String,
}

/// 仅在空白上下文后（句间）或词尾空格后生成 `✦` 续写预测，避免正在拼写某词前半截时高频空转。
pub fn build_continuation_plan(context_before: &str, preedit: &str) -> Option<ContinuationPlan> {
    let ctx_tail = trim_context_tail(context_before, MAX_CONTEXT_CHARS);

    if preedit.is_empty() {
        let prompt = ctx_tail.trim_end_matches([' ', '\t']);
        if prompt.is_empty() {
            return None;
        }
        return Some(ContinuationPlan {
            prompt: prompt.to_string(),
            expect_leading_space: !prompt.ends_with(['\n', '\r']),
            join_base: String::new(),
        });
    }

    if preedit.ends_with(' ') {
        let base_trimmed = preedit.trim_end_matches(' ');
        if base_trimmed.is_empty() {
            return None;
        }
        let mut prompt = String::with_capacity(ctx_tail.len() + base_trimmed.len());
        prompt.push_str(ctx_tail);
        prompt.push_str(base_trimmed);
        return Some(ContinuationPlan {
            prompt,
            expect_leading_space: true,
            join_base: format!("{base_trimmed} "),
        });
    }

    None
}

/// 从续写原始生成的片段中提取单个干净英文词；遇到控制符、换行或非词字符即截断拒绝。
pub fn extract_clean_continuation_word(raw_piece: &str, expect_leading_space: bool) -> Option<String> {
    if raw_piece.contains('\n')
        || raw_piece.contains('\r')
        || raw_piece.contains("<|")
        || raw_piece.contains("</s>")
    {
        return None;
    }

    if expect_leading_space && !raw_piece.starts_with(' ') {
        return None;
    }

    let trimmed = raw_piece.trim_start_matches(' ');
    if trimmed.is_empty() {
        return None;
    }

    let mut word = String::new();
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '\'' || ch == '-' {
            word.push(ch);
        } else {
            break;
        }
    }

    let clean = word.trim_matches(|c| c == '\'' || c == '-');
    if clean.len() < 2 || clean.len() > 24 || !clean.bytes().any(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some(clean.to_string())
}

/// 对候选句子的原始长度归一化对数概率做零均值中心化与温和限幅，
/// 确保 L3 有效拉开语境歧义差异，同时不会粗暴推翻确信度极高的 L1 精确命中。
pub fn normalize_rerank_scores(scored: &[(String, f32)], weight: f32) -> Vec<NeuralCandidate> {
    let finite: Vec<f32> = scored
        .iter()
        .map(|(_, s)| *s)
        .filter(|s| s.is_finite())
        .collect();
    if finite.len() < 2 {
        return Vec::new();
    }
    let mean = finite.iter().copied().sum::<f32>() / (finite.len() as f32);

    scored
        .iter()
        .filter_map(|(text, s)| {
            if !s.is_finite() {
                return None;
            }
            let delta = ((*s - mean) * weight).clamp(-4.5, 4.5);
            Some(NeuralCandidate {
                text: text.clone(),
                score: delta,
                is_continuation: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_prefix_normalizes_bpe_word_boundary_space() {
        let (prefix, cands) = split_prefix_and_candidates(
            "Thank you for your ",
            &["form".into(), "from".into(), " ".into()],
        );
        assert_eq!(prefix, "Thank you for your");
        assert_eq!(cands, vec![" form", " from", ""]);

        let (empty_ctx, cands2) = split_prefix_and_candidates("", &["apple".into()]);
        assert_eq!(empty_ctx, "");
        assert_eq!(cands2, vec!["apple"]);
    }

    #[test]
    fn continuation_plan_and_cleaner_filter_noise() {
        assert!(build_continuation_plan("", "app").is_none());
        let plan = build_continuation_plan("Thank you ", "for ").expect("space-ended preedit");
        assert_eq!(plan.prompt, "Thank you for");
        assert_eq!(plan.join_base, "for ");
        assert!(plan.expect_leading_space);

        assert_eq!(
            extract_clean_continuation_word(" your help,", true).as_deref(),
            Some("your")
        );
        assert!(extract_clean_continuation_word("bad\nline", true).is_none());
        assert!(extract_clean_continuation_word("no_leading_space", true).is_none());
        assert!(extract_clean_continuation_word(" a", true).is_none());
    }

    #[test]
    fn normalize_rerank_scores_centers_and_clamps() {
        let out = normalize_rerank_scores(&[("A".into(), -1.0), ("B".into(), -9.0)], 2.0);
        assert_eq!(out.len(), 2);
        assert!((out[0].score - 4.5).abs() < 1e-4);
        assert!((out[1].score + 4.5).abs() < 1e-4);
    }
}
