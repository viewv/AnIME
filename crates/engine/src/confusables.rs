//! 纯 Rust 英文上下文易混词（Real-Word Confusables）与简写展开适配器。
//!
//! 原因注释：L1 词级引擎对合法真词施加重磅 `EXACT_BONUS = 6.5` 防误纠；
//! 在具备多词词格或上文语境时，本模块向 `LexiconProvider::contextual_confusables` 查询数据驱动的
//! 同音/近音/撇号缩写与低代价 QWERTY 邻键易混词（不含静态硬编码易混词列表），
//! 并以可控先验分差与稀有词温和降权交由 L2/L3 上下文模型决策。

use anime_lexicon::LexiconProvider;
use anime_lm_ngram::SlotCandidate;

const SHORTHAND_RELATIVE_PENALTY: f32 = 0.8;

const SHORTHAND_PAIRS: &[(&str, &str)] = &[
    ("hw", "how"),
    ("r", "are"),
    ("u", "you"),
    ("yu", "you"),
    ("ur", "your"),
    ("pls", "please"),
    ("thx", "thanks"),
    ("msg", "message"),
];

fn match_raw_casing(raw_token: &str, canonical: &str) -> String {
    let first_upper = raw_token
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase());
    if !first_upper || canonical.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return canonical.to_string();
    }
    let mut chars = canonical.chars();
    let Some(first) = chars.next() else {
        return canonical.to_string();
    };
    let mut out = String::with_capacity(canonical.len());
    out.push(first.to_ascii_uppercase());
    out.push_str(chars.as_str());
    out
}

fn upsert_slot_candidate(cands: &mut Vec<SlotCandidate>, word: String, target_score: f32) {
    let lower = word.to_ascii_lowercase();
    if let Some(existing) = cands
        .iter_mut()
        .find(|c| c.word.to_ascii_lowercase() == lower)
    {
        if target_score > existing.lex_score {
            existing.lex_score = target_score;
        }
    } else {
        cands.push(SlotCandidate {
            word,
            lex_score: target_score,
        });
    }
}

/// 当具备语境或处于多词词格时，为词槽补充数据驱动的易混真词与常见缩写候选。
pub(crate) fn expand_slot_confusables<L: LexiconProvider>(
    lexicon: &L,
    raw_token: &str,
    cands: &mut Vec<SlotCandidate>,
    allow_expansion: bool,
) {
    if !allow_expansion {
        return;
    }

    let raw_lower = raw_token.trim().to_ascii_lowercase();
    if raw_lower.is_empty() {
        return;
    }

    let slot_max = cands
        .iter()
        .map(|c| c.lex_score)
        .fold(f32::NEG_INFINITY, f32::max);
    let anchor = if slot_max.is_finite() { slot_max } else { 0.0 };

    for &(short, expanded) in SHORTHAND_PAIRS {
        if raw_lower == short {
            let word = match_raw_casing(raw_token, expanded);
            upsert_slot_candidate(cands, word, anchor - SHORTHAND_RELATIVE_PENALTY);
        }
    }

    let mut hits = lexicon.contextual_confusables(&raw_lower, 6);
    // 若原始敲法不在词库（如 youre/theyre/whos 漏敲撇号），尝试以纠错首项（you're/they're/who's）激活易混族（your/their/whose）
    if hits.is_empty() {
        if let Some(first) = cands.first() {
            if !first.word.eq_ignore_ascii_case(&raw_lower) {
                hits = lexicon.contextual_confusables(&first.word, 6);
            }
        }
    }

    for hit in hits {
        if hit.word.eq_ignore_ascii_case(&raw_lower) {
            continue;
        }
        let word = match_raw_casing(raw_token, &hit.word);
        let rarity_pen = (-4.0 - hit.log_freq).clamp(0.0, 2.5) * 0.15;
        let target_score = anchor - hit.relative_penalty - rarity_pen;
        upsert_slot_candidate(cands, word, target_score);
    }
}
