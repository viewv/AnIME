//! 纯 Rust 英文上下文易混词（Real-Word Confusables）与简写展开适配器。
//!
//! 原因注释：L1 词级引擎对合法真词施加重磅 `EXACT_BONUS = 6.5` 防误纠；
//! 在具备多词词格或上文语境时，本模块向 `LexiconProvider::contextual_confusables` 查询数据驱动的
//! 同音/近音/撇号缩写与低代价 QWERTY 邻键易混词（不含静态硬编码易混词列表），
//! 并以可控先验分差与稀有词温和降权交由 L2/L3 上下文模型决策。

use anime_lexicon::LexiconProvider;
use anime_lm_ngram::SlotCandidate;

use crate::profile::ConfusableProfile;

// 分差参数见 `profile::ConfusableProfile`（按单词 / 整句模式各一份）。各项的 Why：
// - 闭合真词的一次编辑近邻（he->the、hat->that、is->this）：用户已敲出合法真词时必须默认保留原词，
//   只有上下文给出明确证据才翻转。基础分差约等于一个低代价易混词（1.25–1.45），再按编辑物理代价递增；
//   近邻比原词更常见时追加顺风惩罚，防止高频功能词（the/to/that）在无语境时吞掉用户原词。
// - 非词 token（o、ou、nd）的近邻按 L1 自然分（log_freq - λ·cost）参与，但不得超过槽内最佳分，
//   避免一次编辑的高频词压过该槽已有的低代价纠错。
// - 用户敲出的真词过于罕见（Zipf < 3）时不展开真词近邻：这类词往往有意为之（专名、术语），
//   且 L2 对其统计稀薄，LM 概率对比会系统性偏向常见近邻而误改用户原词。
// - 单词模式（单 token + 上文）下“单次 QWERTY 直接邻键替换”易混真词对（fast/vast、life/live、bat/vat）
//   有最低先验分差：这类词对只是手指误触关系而非读音 / 用法混淆，误触在两个方向上同样可能，
//   用户已敲出的合法真词没有理由让位于更罕见的邻键词；而单 token 路径叠加的 L2 是与词频无关的
//   PMI 提升（`score_in_context`），会放大罕见词偶见搭配（"the vast"、"that vast"），
//   因此要求 L2 给出至少该下限（默认 3.0）的上下文优势才允许翻盘（"the" 后 vast 的 PMI 提升约 2.4）。

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

/// 两词等长且仅一处替换，替换双方为 QWERTY 直接邻键且不同为元音（元音互换如 in/on、then/than
/// 属于拼写 / 用法混淆，不按误触处理）。
fn is_adjacent_key_substitution(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = None;
    for (x, y) in a.bytes().zip(b.bytes()) {
        if x != y {
            if diff.is_some() {
                return false;
            }
            diff = Some((x, y));
        }
    }
    diff.is_some_and(|(x, y)| {
        anime_lexicon::keyboard::is_direct_neighbor(x, y)
            && !(is_ascii_vowel(x) && is_ascii_vowel(y))
    })
}

fn is_ascii_vowel(b: u8) -> bool {
    matches!(b.to_ascii_lowercase(), b'a' | b'e' | b'i' | b'o' | b'u')
}

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

/// 为整句模式中已闭合的 token 补充一次编辑近邻候选（召回入口见 `LexiconProvider::closed_token_neighbors`）。
///
/// `exact_anchor_bonus` 是精确词基准分中的固定加成（L1 `EXACT_BONUS` + profile 精确词偏移），据此从锚点还原原词词频。
pub(crate) fn expand_closed_token_neighbors(
    neighbors: &[anime_lexicon::LexiconCandidate],
    raw_token: &str,
    raw_is_word: bool,
    cands: &mut Vec<SlotCandidate>,
    p: &ConfusableProfile,
    exact_anchor_bonus: f32,
) {
    let slot_max = cands
        .iter()
        .map(|c| c.lex_score)
        .fold(f32::NEG_INFINITY, f32::max);
    let anchor = if slot_max.is_finite() { slot_max } else { 0.0 };
    // 精确词基准分为 log_freq + EXACT_BONUS(6.5)，据此还原原词词频
    let raw_log_freq = (anchor - exact_anchor_bonus).clamp(-8.5, -1.0);
    if raw_is_word && raw_log_freq < p.realword_neighbor_min_raw_log_freq {
        return;
    }
    for nb in neighbors.iter().take(p.max_closed_neighbors) {
        if nb.word.eq_ignore_ascii_case(raw_token) {
            continue;
        }
        let target = if raw_is_word {
            let surplus = (nb.log_freq - raw_log_freq).clamp(0.0, 2.0);
            anchor
                - p.realword_neighbor_base_penalty
                - p.realword_neighbor_cost_weight * nb.edit_cost
                - p.realword_neighbor_surplus_weight * surplus
        } else if slot_max.is_finite() {
            nb.score.min(slot_max - p.nonword_neighbor_cap_margin)
        } else {
            nb.score
        };
        upsert_slot_candidate(cands, match_raw_casing(raw_token, &nb.word), target);
    }
}

/// 当具备语境或处于多词词格时，为词槽补充数据驱动的易混真词与常见缩写候选。
///
/// `single_token` 为真表示单词模式（单 token + 上文）：候选随后只叠加 `score_in_context`
/// 的 PMI 提升而非词格中的完整条件概率，邻键误触词对改用 `adjacent_key_single_token_min_penalty`。
pub(crate) fn expand_slot_confusables<L: LexiconProvider>(
    lexicon: &L,
    raw_token: &str,
    cands: &mut Vec<SlotCandidate>,
    allow_expansion: bool,
    single_token: bool,
    p: &ConfusableProfile,
    exact_anchor_bonus: f32,
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
            upsert_slot_candidate(cands, word, anchor - p.shorthand_penalty);
        }
    }

    let mut hits = lexicon.contextual_confusables(&raw_lower, p.query_limit);
    // 若原始敲法不在词库（如 youre/theyre/whos 漏敲撇号），尝试以纠错首项（you're/they're/who's）激活易混族（your/their/whose）
    if hits.is_empty() {
        if let Some(first) = cands.first() {
            if !first.word.eq_ignore_ascii_case(&raw_lower) {
                hits = lexicon.contextual_confusables(&first.word, p.query_limit);
            }
        }
    }

    // 精确词在 L1 中的基准分为 log_freq + EXACT_BONUS(6.5)，据此还原原词对数词频用于对称频差补偿：
    // - 当易混候选词频高于用户已敲出的合法词（如 quiet->quite, suite->sweet）时，附加顺风惩罚防止无语境误翻；
    // - 当易混候选词频低于用户敲出的高频词（如 break->brake, night->knight, colonel->kernel）时，适度减免先验惩罚以抵消 L2/L3 的 unigram 劣势。
    let est_base_log_freq = (anchor - exact_anchor_bonus).clamp(-8.5, -1.0);
    let raw_has_apos = raw_lower.contains('\'');

    for hit in hits {
        if hit.word.eq_ignore_ascii_case(&raw_lower) {
            continue;
        }
        let word = match_raw_casing(raw_token, &hit.word);
        let is_apos_expansion = !raw_has_apos && hit.word.contains('\'');
        let effective_penalty = if is_apos_expansion {
            hit.relative_penalty
        } else if hit.log_freq > est_base_log_freq {
            let surplus_pen =
                (hit.log_freq - est_base_log_freq).clamp(0.0, p.surplus_clamp) * p.surplus_weight;
            hit.relative_penalty + surplus_pen
        } else {
            let deficit_relief =
                (est_base_log_freq - hit.log_freq).clamp(0.0, p.deficit_clamp) * p.deficit_weight;
            (hit.relative_penalty - deficit_relief).max(p.min_penalty)
        };
        let effective_penalty = if single_token
            && is_adjacent_key_substitution(&raw_lower, &hit.word.to_ascii_lowercase())
        {
            effective_penalty.max(p.adjacent_key_single_token_min_penalty)
        } else {
            effective_penalty
        };
        let target_score = anchor - effective_penalty;
        upsert_slot_candidate(cands, word, target_score);
    }
}
