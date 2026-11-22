//! 英文漏撇号缩写恢复与大小写规范化辅助逻辑。

#[derive(Debug, Clone, Copy)]
pub struct ContractionRule {
    pub raw_lower: &'static str,
    pub expanded: &'static str,
    pub collides_with_word: bool,
    pub assumed_log_freq: f32,
}

pub const CONTRACTION_RULES: &[ContractionRule] = &[
    ContractionRule { raw_lower: "im", expanded: "I'm", collides_with_word: false, assumed_log_freq: -2.3 },
    ContractionRule { raw_lower: "ive", expanded: "I've", collides_with_word: false, assumed_log_freq: -2.8 },
    ContractionRule { raw_lower: "dont", expanded: "don't", collides_with_word: false, assumed_log_freq: -2.2 },
    ContractionRule { raw_lower: "cant", expanded: "can't", collides_with_word: false, assumed_log_freq: -2.4 },
    ContractionRule { raw_lower: "wont", expanded: "won't", collides_with_word: false, assumed_log_freq: -2.8 },
    ContractionRule { raw_lower: "isnt", expanded: "isn't", collides_with_word: false, assumed_log_freq: -2.9 },
    ContractionRule { raw_lower: "arent", expanded: "aren't", collides_with_word: false, assumed_log_freq: -3.1 },
    ContractionRule { raw_lower: "didnt", expanded: "didn't", collides_with_word: false, assumed_log_freq: -2.6 },
    ContractionRule { raw_lower: "doesnt", expanded: "doesn't", collides_with_word: false, assumed_log_freq: -2.7 },
    ContractionRule { raw_lower: "wasnt", expanded: "wasn't", collides_with_word: false, assumed_log_freq: -3.0 },
    ContractionRule { raw_lower: "werent", expanded: "weren't", collides_with_word: false, assumed_log_freq: -3.4 },
    ContractionRule { raw_lower: "havent", expanded: "haven't", collides_with_word: false, assumed_log_freq: -3.0 },
    ContractionRule { raw_lower: "hasnt", expanded: "hasn't", collides_with_word: false, assumed_log_freq: -3.3 },
    ContractionRule { raw_lower: "hadnt", expanded: "hadn't", collides_with_word: false, assumed_log_freq: -3.5 },
    ContractionRule { raw_lower: "wouldnt", expanded: "wouldn't", collides_with_word: false, assumed_log_freq: -2.9 },
    ContractionRule { raw_lower: "couldnt", expanded: "couldn't", collides_with_word: false, assumed_log_freq: -3.0 },
    ContractionRule { raw_lower: "shouldnt", expanded: "shouldn't", collides_with_word: false, assumed_log_freq: -3.2 },
    ContractionRule { raw_lower: "mustnt", expanded: "mustn't", collides_with_word: false, assumed_log_freq: -3.8 },
    ContractionRule { raw_lower: "neednt", expanded: "needn't", collides_with_word: false, assumed_log_freq: -4.1 },
    ContractionRule { raw_lower: "theyre", expanded: "they're", collides_with_word: false, assumed_log_freq: -2.7 },
    ContractionRule { raw_lower: "youre", expanded: "you're", collides_with_word: false, assumed_log_freq: -2.5 },
    ContractionRule { raw_lower: "thats", expanded: "that's", collides_with_word: false, assumed_log_freq: -2.4 },
    ContractionRule { raw_lower: "theres", expanded: "there's", collides_with_word: false, assumed_log_freq: -2.7 },
    ContractionRule { raw_lower: "whats", expanded: "what's", collides_with_word: false, assumed_log_freq: -2.6 },
    ContractionRule { raw_lower: "whos", expanded: "who's", collides_with_word: false, assumed_log_freq: -3.3 },
    ContractionRule { raw_lower: "lets", expanded: "let's", collides_with_word: false, assumed_log_freq: -2.9 },
    ContractionRule { raw_lower: "youve", expanded: "you've", collides_with_word: false, assumed_log_freq: -3.1 },
    ContractionRule { raw_lower: "weve", expanded: "we've", collides_with_word: false, assumed_log_freq: -3.2 },
    ContractionRule { raw_lower: "theyve", expanded: "they've", collides_with_word: false, assumed_log_freq: -3.4 },
    ContractionRule { raw_lower: "youll", expanded: "you'll", collides_with_word: false, assumed_log_freq: -3.2 },
    ContractionRule { raw_lower: "theyll", expanded: "they'll", collides_with_word: false, assumed_log_freq: -3.4 },
    ContractionRule { raw_lower: "theyd", expanded: "they'd", collides_with_word: false, assumed_log_freq: -3.6 },
    ContractionRule { raw_lower: "youd", expanded: "you'd", collides_with_word: false, assumed_log_freq: -3.5 },
    ContractionRule { raw_lower: "hes", expanded: "he's", collides_with_word: false, assumed_log_freq: -2.8 },
    ContractionRule { raw_lower: "shes", expanded: "she's", collides_with_word: false, assumed_log_freq: -3.0 },
    ContractionRule { raw_lower: "wheres", expanded: "where's", collides_with_word: false, assumed_log_freq: -3.4 },
    ContractionRule { raw_lower: "heres", expanded: "here's", collides_with_word: false, assumed_log_freq: -3.1 },
    ContractionRule { raw_lower: "hows", expanded: "how's", collides_with_word: false, assumed_log_freq: -3.4 },
    ContractionRule { raw_lower: "whens", expanded: "when's", collides_with_word: false, assumed_log_freq: -3.8 },
    ContractionRule { raw_lower: "whys", expanded: "why's", collides_with_word: false, assumed_log_freq: -3.9 },
    ContractionRule { raw_lower: "aint", expanded: "ain't", collides_with_word: false, assumed_log_freq: -3.3 },
    // 与合法英文单词碰撞的缩写：仅作强次优候选展示，绝不挤掉真实词典精确词
    ContractionRule { raw_lower: "ill", expanded: "I'll", collides_with_word: true, assumed_log_freq: -2.6 },
    ContractionRule { raw_lower: "id", expanded: "I'd", collides_with_word: true, assumed_log_freq: -2.8 },
    ContractionRule { raw_lower: "its", expanded: "it's", collides_with_word: true, assumed_log_freq: -2.2 },
    ContractionRule { raw_lower: "were", expanded: "we're", collides_with_word: true, assumed_log_freq: -2.7 },
    ContractionRule { raw_lower: "well", expanded: "we'll", collides_with_word: true, assumed_log_freq: -3.0 },
    ContractionRule { raw_lower: "wed", expanded: "we'd", collides_with_word: true, assumed_log_freq: -3.5 },
    ContractionRule { raw_lower: "hell", expanded: "he'll", collides_with_word: true, assumed_log_freq: -3.3 },
    ContractionRule { raw_lower: "shell", expanded: "she'll", collides_with_word: true, assumed_log_freq: -3.6 },
    ContractionRule { raw_lower: "hed", expanded: "he'd", collides_with_word: true, assumed_log_freq: -3.6 },
    ContractionRule { raw_lower: "shed", expanded: "she'd", collides_with_word: true, assumed_log_freq: -3.8 },
];

pub fn lookup_contraction(lower: &str) -> Option<&'static ContractionRule> {
    CONTRACTION_RULES.iter().find(|r| r.raw_lower == lower)
}

/// 根据用户输入的首字母大写或全大写习惯恢复候选词大小写（保留专有名词与混合大小写词形）。
pub fn apply_input_casing(raw_trimmed: &str, canonical: &str, is_proper_or_mixed: bool) -> String {
    if is_proper_or_mixed {
        return canonical.to_string();
    }
    let alpha_count = raw_trimmed
        .bytes()
        .filter(|b| b.is_ascii_alphabetic())
        .count();
    let all_upper = alpha_count >= 2
        && raw_trimmed
            .bytes()
            .filter(|b| b.is_ascii_alphabetic())
            .all(|b| b.is_ascii_uppercase());
    if all_upper {
        return canonical.to_ascii_uppercase();
    }
    let first_upper = raw_trimmed
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase());
    if first_upper {
        let mut chars = canonical.chars();
        if let Some(first) = chars.next() {
            let mut out = String::with_capacity(canonical.len());
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
            return out;
        }
    }
    canonical.to_string()
}
