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

fn longest_shared_word_prefix<'a>(non_empty: &[&'a str]) -> Option<&'a str> {
    if non_empty.len() < 2 {
        return None;
    }
    let first = non_empty[0];
    let bytes = first.as_bytes();
    let mut best_end: Option<usize> = None;
    for (idx, &b) in bytes.iter().enumerate() {
        if b != b' ' || idx == 0 || bytes[idx - 1] == b' ' {
            continue;
        }
        let with_space_len = idx + 1;
        let with_space = &first[..with_space_len];
        let all_match = non_empty.iter().all(|&cand| {
            cand.starts_with(with_space)
                && !cand[with_space_len..]
                    .trim_start_matches([' ', '\t'])
                    .is_empty()
        });
        if all_match {
            best_end = Some(idx);
        }
    }
    best_end.map(|end| &first[..end])
}

/// 将 `context_before` 与候选列表规整为 BPE 友好的 `(shared_prefix, candidate_suffixes)`：
/// 1. 英文 BPE 分词器习惯将词前空格并入词 token（如 `Ġapple`），将前缀末尾空格挪至各候选开头避免孤立空格 token 扭曲概率；
/// 2. 将所有非空候选共同拥有的整词边界前缀折叠进 `shared_prefix`——同一共同前缀的对数概率为常数，
///    在零均值中心化 `normalize_rerank_scores` 后精确抵消，同时能复用前缀 KV-Cache 把多词整句后缀降至接近单步 O(1) decode。
pub fn split_prefix_and_candidates(
    context_before: &str,
    candidates: &[String],
) -> (String, Vec<String>) {
    let tail = trim_context_tail(context_before, MAX_CONTEXT_CHARS);
    let has_trailing_space = tail.ends_with(' ') || tail.ends_with('\t');
    let trimmed_prefix = tail.trim_end_matches([' ', '\t']);
    let base_needs_space =
        has_trailing_space && !trimmed_prefix.is_empty() && !trimmed_prefix.ends_with(['\n', '\r']);

    let cores: Vec<&str> = candidates.iter().map(|cand| cand.trim()).collect();
    let non_empty: Vec<&str> = cores.iter().copied().filter(|c| !c.is_empty()).collect();
    let shared_words = longest_shared_word_prefix(&non_empty);

    let (final_prefix, final_needs_space) = if let Some(words) = shared_words {
        let mut combined = String::with_capacity(trimmed_prefix.len() + 1 + words.len());
        combined.push_str(trimmed_prefix);
        if base_needs_space {
            combined.push(' ');
        }
        combined.push_str(words);
        (combined, true)
    } else {
        (trimmed_prefix.to_string(), base_needs_space)
    };

    let formatted = cores
        .into_iter()
        .map(|core| {
            if core.is_empty() {
                String::new()
            } else {
                let tail_piece = if let Some(words) = shared_words {
                    core[words.len()..].trim_start_matches([' ', '\t'])
                } else {
                    core
                };
                if final_needs_space {
                    format!(" {tail_piece}")
                } else {
                    tail_piece.to_string()
                }
            }
        })
        .collect();

    (final_prefix, formatted)
}

/// 专用纠错模型的提示模板（训练数据构建脚本 `scripts/data_pipeline/build_l3_sft.py` 必须逐字节一致）：
///
/// ```text
/// <|ctx|> {上文}\n<|typed|> {键入原文}\n<|fix|> {目标文本}<|end|>
/// ```
///
/// 原因注释：标记均为普通文本（不新增特殊 token），GGUF 与分词器零改动即可加载微调权重；
/// 目标末尾带 `<|end|>`，使不同长度的整句候选在同一终止条件下比较联合概率，避免短候选占便宜。
pub const CORRECTION_CTX_MARK: &str = "<|ctx|>";
pub const CORRECTION_TYPED_MARK: &str = "<|typed|>";
pub const CORRECTION_FIX_MARK: &str = "<|fix|>";
pub const CORRECTION_END_MARK: &str = "<|end|>";
/// 上文只保留末尾若干字符，保证模板标记永不被 token 截断吃掉。
pub const CORRECTION_MAX_CONTEXT_CHARS: usize = 160;

pub fn build_correction_prompt(context_before: &str, typed: &str) -> String {
    let ctx = trim_context_tail(context_before, CORRECTION_MAX_CONTEXT_CHARS);
    let ctx = ctx.split_whitespace().collect::<Vec<_>>().join(" ");
    let typed = typed.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{CORRECTION_CTX_MARK} {ctx}\n{CORRECTION_TYPED_MARK} {typed}\n{CORRECTION_FIX_MARK}")
}

pub fn correction_target(candidate: &str) -> String {
    let cand = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(" {cand}{CORRECTION_END_MARK}")
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

        // 候选整句共同词边界前缀折叠进 shared_prefix
        let (fold_prefix, fold_cands) = split_prefix_and_candidates(
            "",
            &["we didn't see the bug".into(), "we didn't see the big".into()],
        );
        assert_eq!(fold_prefix, "we didn't see the");
        assert_eq!(fold_cands, vec![" bug", " big"]);

        let (fold_ctx, fold_ctx_cands) = split_prefix_and_candidates(
            "Done. ",
            &["The build passed".into(), "The build failed".into()],
        );
        assert_eq!(fold_ctx, "Done. The build");
        assert_eq!(fold_ctx_cands, vec![" passed", " failed"]);

        let (partial_prefix, partial_cands) = split_prefix_and_candidates(
            "",
            &["we see the".into(), "we see the bug".into()],
        );
        assert_eq!(partial_prefix, "we see");
        assert_eq!(partial_cands, vec![" the", " the bug"]);
    }

    #[test]
    fn correction_template_matches_training_golden() {
        // 与 scripts/data_pipeline/test_pipeline.py::TestL3Sft 中的黄金样例逐字节一致
        let prompt = build_correction_prompt("  Can you  help me ", "i want o  go");
        assert_eq!(prompt, "<|ctx|> Can you help me\n<|typed|> i want o go\n<|fix|>");
        assert_eq!(correction_target(" I want to go "), " I want to go<|end|>");
        assert_eq!(build_correction_prompt("", "hello"), "<|ctx|> \n<|typed|> hello\n<|fix|>");
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
