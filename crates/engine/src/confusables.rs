//! 纯 Rust 英文上下文易混词（Real-Word Confusables）与简写展开表。
//!
//! 原因注释：L1 词级引擎对合法真词施加重磅 `EXACT_BONUS = 6.5` 防误纠；
//! 在具备多词词格或上文语境时，本模块只为明确登记的易混搭档补充可控先验分差（`-1.35`），
//! 使 L2 2/3-gram 能在 `"go form here"` / `"over their"` / `"I hope your happy"` 中精准翻转，
//! 同时对无强语境支撑或非易混生僻词保持零误改。

use anime_lm_ngram::SlotCandidate;

const CONFUSABLE_RELATIVE_PENALTY: f32 = 1.35;
const SHORTHAND_RELATIVE_PENALTY: f32 = 0.8;

const CONFUSABLE_GROUPS: &[&[&str]] = &[
    &["their", "there", "they're"],
    &["your", "you're"],
    &["its", "it's"],
    &["to", "too", "two"],
    &["then", "than"],
    &["form", "from"],
    &["affect", "effect"],
    &["lose", "loose"],
    &["weather", "whether"],
    &["quiet", "quite"],
    &["accept", "except"],
    &["here", "hear"],
    &["know", "no"],
    &["new", "knew"],
    &["right", "write"],
    &["where", "wear"],
    &["passed", "past"],
    &["peace", "piece"],
    &["break", "brake"],
    &["principal", "principle"],
    &["complement", "compliment"],
    &["desert", "dessert"],
    &["ill", "I'll"],
    &["id", "I'd"],
    &["were", "we're"],
    &["well", "we'll"],
    &["cant", "can't"],
    &["wont", "won't"],
    &["dont", "don't"],
    &["isnt", "isn't"],
    &["didnt", "didn't"],
    &["doesnt", "doesn't"],
    &["thats", "that's"],
    &["whats", "what's"],
    &["hes", "he's"],
    &["shes", "she's"],
];

const SHORTHAND_PAIRS: &[(&str, &str)] = &[
    ("hw", "how"),
    ("r", "are"),
    ("u", "you"),
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

/// 当具备语境或处于多词词格时，为词槽补充易混真词与常见缩写候选。
pub(crate) fn expand_slot_confusables(
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

    // 同时依据原始输入与前 3 项已有候选激活易混组（例如 youre -> you're 激活 your）
    let probe_limit = cands.len().min(3);
    let mut probes: Vec<String> = Vec::with_capacity(1 + probe_limit);
    probes.push(raw_lower.clone());
    for c in &cands[..probe_limit] {
        probes.push(c.word.to_ascii_lowercase());
    }

    let confusable_score = anchor - CONFUSABLE_RELATIVE_PENALTY;
    for group in CONFUSABLE_GROUPS {
        let matched = probes.iter().any(|p| {
            group
                .iter()
                .any(|&member| member.eq_ignore_ascii_case(p))
        });
        if !matched {
            continue;
        }
        for &member in *group {
            if member.eq_ignore_ascii_case(&raw_lower) {
                continue;
            }
            let word = match_raw_casing(raw_token, member);
            upsert_slot_candidate(cands, word, confusable_score);
        }
    }
}
