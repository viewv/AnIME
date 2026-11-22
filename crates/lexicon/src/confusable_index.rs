//! 数据驱动的上下文易混真词离线索引（零运行时 Trie 模糊搜索、零手写词表）：
//! 1) 词库自动撇号缩写配对（its/it's、lets/let's、were/we're、whose/who's 等高频缩写）
//! 2) CMUdict 精确同音词（homophones，如 right/write、colonel/kernel、there/their/they're）
//! 3) CMUdict 擦音清浊骨架 + 美式卷舌音中性化近音词（accept/except、personal/personnel、advice/advise、breath/breathe、moral/morale）
//! 4) 词典内部自发拼写近似真词对（双写增删 of/off、to/too、lose/loose；相邻换位 form/from、quiet/quite；词尾 /n/-/nd/ an/and；高频极短邻键 in/on、has/had）
//! 5) 无 CMUdict 测试词库下的 Double Metaphone Tier-0 + 编辑代价兜底

use std::collections::{HashMap, HashSet};

use crate::contractions::lookup_contraction;
use crate::keyboard::{substitution_cost, weighted_damerau_levenshtein};
use crate::phonetic::encode_double_metaphone;

/// 索引收词最低对数词频（约对应 Zipf >= 2.5，足以覆盖 elicit/stationery/kernel 等低频易混书面词）。
const MIN_INDEX_LOG_FREQ: f32 = -6.5;
/// 精确同音与近音桶收词最低对数词频（约 Zipf >= 2.8）。
const MIN_PHONETIC_LOG_FREQ: f32 = -6.2;
/// 非缩写专有名词/混写词若词频低于此阈值（Zipf < 5.5）则不作为普通小写词的易混目标，避免人名/地名污染。
const MIN_PROPER_TARGET_LOG_FREQ: f32 = -3.5;
/// 非对称词频差上限：防止超高频基础词（如 the）连向冷僻同音词（如 thee）。
const MAX_TARGET_FREQ_DROP: f32 = 3.0;
/// 撇号缩写目标的最低词频门槛（Zipf >= 4.2），避免稀有所有格 `'s` 污染普通复数词。
const MIN_APOSTROPHE_CONTRACTION_LOG_FREQ: f32 = -4.8;
const MIN_APOSTROPHE_POSSESSIVE_S_LOG_FREQ: f32 = -4.2;

/// 每个桶参与成对比较的上限（先按词频降序截断），保障 12 万词加载期建表 < 50ms。
const MAX_EXACT_HOMOPHONE_BUCKET: usize = 16;
const MAX_SKELETON_BUCKET: usize = 64;
const MAX_STATIC_NEIGHBORS_PER_WORD: usize = 16;

/// 上下文易混真词候选。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfusableCandidate {
    pub word: String,
    pub log_freq: f32,
    /// 相对原输入词的基准纠错惩罚（越低越容易被 L2/L3 上下文翻盘）。
    pub relative_penalty: f32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConfusableSourceEntry<'a> {
    pub word_id: u32,
    pub word: &'a str,
    pub lower: &'a str,
    pub log_freq: f32,
    pub is_proper_or_mixed: bool,
    pub from_scowl: bool,
    pub phonetic: &'a str,
}

#[derive(Debug, Clone)]
pub(crate) struct ConfusableEdge {
    pub target_id: u32,
    pub relative_penalty: f32,
}

#[derive(Debug, Clone)]
struct IndexedWord {
    word_id: u32,
    lower: String,
    log_freq: f32,
    is_proper_or_mixed: bool,
    from_scowl: bool,
    has_apostrophe: bool,
    is_pure_contraction_typo: bool,
    alpha_mask: u32,
    prons: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RealWordConfusableIndex {
    edges_by_word: Vec<Vec<ConfusableEdge>>,
}

impl RealWordConfusableIndex {
    pub(crate) fn empty(num_words: usize) -> Self {
        Self {
            edges_by_word: vec![Vec::new(); num_words],
        }
    }

    pub(crate) fn build(entries: &[ConfusableSourceEntry<'_>]) -> Self {
        let num_words = entries.len();
        if num_words == 0 {
            return Self::empty(0);
        }

        let has_cmudict = entries.iter().any(|e| !e.phonetic.is_empty());
        let mut indexed: Vec<Option<IndexedWord>> = vec![None; num_words];

        let mut apos_buckets: HashMap<String, Vec<u32>> = HashMap::new();
        let mut exact_homophone_buckets: HashMap<String, Vec<u32>> = HashMap::new();
        let mut skeleton_buckets: HashMap<String, Vec<u32>> = HashMap::new();
        let mut metaphone_buckets: HashMap<String, Vec<u32>> = HashMap::new();

        for entry in entries {
            if !is_eligible_entry(entry) {
                continue;
            }
            let has_apostrophe = entry.lower.contains('\'');
            // 如 im / cant / dont / wont 等在 contractions 表中属于无歧义漏撇号简写（!collides_with_word），
            // 它们只应参与撇号恢复（im->I'm），不应在同音桶中误吸其它词（如 im->him, cant->kant）。
            let is_pure_contraction_typo = !has_apostrophe
                && lookup_contraction(entry.lower).is_some_and(|r| !r.collides_with_word);
            let prons = parse_pronunciations(entry.phonetic);
            let alpha_mask = ascii_alpha_mask(entry.lower);
            let wid = entry.word_id;

            // 1. 自动撇号缩写桶：去掉 '\'' 后相同即归组（its/it's、lets/let's、were/we're、ill/I'll 等）
            let stripped: String = entry
                .lower
                .bytes()
                .filter(|&b| b != b'\'')
                .map(|b| b as char)
                .collect();
            if stripped.len() >= 2 {
                apos_buckets.entry(stripped).or_default().push(wid);
            }

            if has_cmudict {
                if !is_pure_contraction_typo
                    && entry.from_scowl
                    && !prons.is_empty()
                    && entry.log_freq >= MIN_PHONETIC_LOG_FREQ
                {
                    let mut seen_exact = HashSet::new();
                    let mut seen_skel = HashSet::new();
                    for raw_pron in entry.phonetic.split('|') {
                        let norm_pron = raw_pron.trim();
                        if norm_pron.is_empty() {
                            continue;
                        }
                        if seen_exact.insert(norm_pron.to_string()) {
                            exact_homophone_buckets
                                .entry(norm_pron.to_string())
                                .or_default()
                                .push(wid);
                        }
                    }
                    if entry.lower.len() >= 3 {
                        for pron in &prons {
                            let skel = consonant_voicing_skeleton(pron);
                            if skel.len() >= 2 && seen_skel.insert(skel.clone()) {
                                skeleton_buckets.entry(skel).or_default().push(wid);
                            }
                        }
                    }
                }
            } else if !is_pure_contraction_typo && entry.lower.len() >= 3 {
                // 无 CMUdict（小规模内存测试词库）时退化到 Double Metaphone Tier-0 桶
                let plain: String = entry
                    .lower
                    .bytes()
                    .filter(|b| b.is_ascii_alphabetic())
                    .map(|b| b as char)
                    .collect();
                let (p, s) = encode_double_metaphone(&plain);
                if !p.is_empty() {
                    metaphone_buckets.entry(p.clone()).or_default().push(wid);
                }
                if let Some(sec) = s {
                    if !sec.is_empty() && sec != p {
                        metaphone_buckets.entry(sec).or_default().push(wid);
                    }
                }
            }

            indexed[wid as usize] = Some(IndexedWord {
                word_id: wid,
                lower: entry.lower.to_string(),
                log_freq: entry.log_freq,
                is_proper_or_mixed: entry.is_proper_or_mixed,
                from_scowl: entry.from_scowl,
                has_apostrophe,
                is_pure_contraction_typo,
                alpha_mask,
                prons,
            });
        }

        let mut pair_best: Vec<HashMap<u32, f32>> = vec![HashMap::new(); num_words];

        // 1) 撇号缩写互换边（过滤低频所有格 's，对漏打撇号且目标词频更高的常见缩写如 lets->let's / ill->I'll 降低惩罚）
        for bucket in apos_buckets.values_mut() {
            if bucket.len() < 2 {
                continue;
            }
            let any_apos = bucket
                .iter()
                .filter_map(|&id| indexed[id as usize].as_ref())
                .any(|w| w.has_apostrophe);
            if !any_apos {
                continue;
            }
            for i in 0..bucket.len() {
                for j in (i + 1)..bucket.len() {
                    let a_id = bucket[i];
                    let b_id = bucket[j];
                    let (Some(a), Some(b)) = (&indexed[a_id as usize], &indexed[b_id as usize]) else {
                        continue;
                    };
                    if a.has_apostrophe == b.has_apostrophe {
                        continue;
                    }
                    let (plain_w, apost_w) = if a.has_apostrophe { (b, a) } else { (a, b) };
                    let min_apost_freq = if apost_w.lower.ends_with("'s") {
                        MIN_APOSTROPHE_POSSESSIVE_S_LOG_FREQ
                    } else {
                        MIN_APOSTROPHE_CONTRACTION_LOG_FREQ
                    };
                    if apost_w.log_freq < min_apost_freq {
                        continue;
                    }
                    let freq_adv = (apost_w.log_freq - plain_w.log_freq).clamp(0.0, 1.5);
                    let to_apost_pen = (0.85 - 0.35 * freq_adv).max(0.35);
                    let to_plain_pen = 1.15;
                    record_directed_edge(&mut pair_best, plain_w, apost_w, to_apost_pen);
                    record_directed_edge(&mut pair_best, apost_w, plain_w, to_plain_pen);
                }
            }
        }

        if has_cmudict {
            // 2) CMUdict 精确同音词边
            for bucket in exact_homophone_buckets.values_mut() {
                if bucket.len() < 2 {
                    continue;
                }
                if bucket.len() > MAX_EXACT_HOMOPHONE_BUCKET {
                    bucket.sort_by(|&x, &y| {
                        let fx = indexed[x as usize].as_ref().map(|w| w.log_freq).unwrap_or(-99.0);
                        let fy = indexed[y as usize].as_ref().map(|w| w.log_freq).unwrap_or(-99.0);
                        fy.total_cmp(&fx)
                    });
                    bucket.truncate(MAX_EXACT_HOMOPHONE_BUCKET);
                }
                for i in 0..bucket.len() {
                    for j in (i + 1)..bucket.len() {
                        let (Some(a), Some(b)) =
                            (&indexed[bucket[i] as usize], &indexed[bucket[j] as usize])
                        else {
                            continue;
                        };
                        if are_us_uk_spelling_variants(&a.lower, &b.lower) {
                            continue;
                        }
                        let orth = weighted_damerau_levenshtein(&a.lower, &b.lower);
                        let penalty = 1.25 + 0.05 * orth.min(3.0);
                        record_symmetric_edge(&mut pair_best, a, b, penalty);
                    }
                }
            }

            // 3) CMUdict 擦音清浊骨架 + 加权音素编辑距离 近音词边
            for bucket in skeleton_buckets.values_mut() {
                if bucket.len() < 2 {
                    continue;
                }
                if bucket.len() > MAX_SKELETON_BUCKET {
                    bucket.sort_by(|&x, &y| {
                        let fx = indexed[x as usize].as_ref().map(|w| w.log_freq).unwrap_or(-99.0);
                        let fy = indexed[y as usize].as_ref().map(|w| w.log_freq).unwrap_or(-99.0);
                        fy.total_cmp(&fx)
                    });
                    bucket.truncate(MAX_SKELETON_BUCKET);
                }
                for i in 0..bucket.len() {
                    for j in (i + 1)..bucket.len() {
                        let (Some(a), Some(b)) =
                            (&indexed[bucket[i] as usize], &indexed[bucket[j] as usize])
                        else {
                            continue;
                        };
                        let len_a = a.lower.len();
                        let len_b = b.lower.len();
                        let min_len = len_a.min(len_b);
                        if min_len < 3 || len_a.abs_diff(len_b) > 2 {
                            continue;
                        }
                        if (a.alpha_mask ^ b.alpha_mask).count_ones() > 4 {
                            continue;
                        }
                        if are_us_uk_spelling_variants(&a.lower, &b.lower) {
                            continue;
                        }
                        let orth = weighted_damerau_levenshtein(&a.lower, &b.lower);
                        let max_orth = if min_len <= 4 { 0.90 } else { 1.65 };
                        if orth > max_orth {
                            continue;
                        }
                        let Some(phon_dist) =
                            best_compatible_pron_distance(a, b)
                        else {
                            continue;
                        };
                        let accepted = if min_len <= 4 {
                            phon_dist <= 0.60 && orth <= 0.90
                        } else {
                            phon_dist <= 0.85 && orth <= 1.65
                        };
                        if !accepted {
                            continue;
                        }
                        let penalty = (1.28 + 0.25 * phon_dist + 0.08 * orth).min(1.70);
                        record_symmetric_edge(&mut pair_best, a, b, penalty);
                    }
                }
            }
        } else {
            // 4) 无 CMUdict 时的 Double Metaphone Tier-0 + 编辑代价兜底
            for bucket in metaphone_buckets.values_mut() {
                if bucket.len() < 2 {
                    continue;
                }
                if bucket.len() > MAX_SKELETON_BUCKET {
                    bucket.truncate(MAX_SKELETON_BUCKET);
                }
                for i in 0..bucket.len() {
                    for j in (i + 1)..bucket.len() {
                        let (Some(a), Some(b)) =
                            (&indexed[bucket[i] as usize], &indexed[bucket[j] as usize])
                        else {
                            continue;
                        };
                        if a.lower.len().abs_diff(b.lower.len()) > 2 {
                            continue;
                        }
                        let orth = weighted_damerau_levenshtein(&a.lower, &b.lower);
                        if orth <= 1.25 {
                            let penalty = 1.15 + 0.15 * orth;
                            record_symmetric_edge(&mut pair_best, a, b, penalty);
                        }
                    }
                }
            }
        }

        // 5) 离线拼写近似生成器（与有无 CMUdict 无关，基于词典自身闭集精确查找）
        let mut lower_to_id: HashMap<&str, u32> = HashMap::with_capacity(num_words);
        for w in indexed.iter().flatten() {
            if !w.is_proper_or_mixed && !w.has_apostrophe && w.from_scowl && !w.is_pure_contraction_typo {
                lower_to_id.insert(w.lower.as_str(), w.word_id);
            }
        }

        // 5a) 双写增删真词对：辅音双写（of/off, later/latter, desert/dessert）直接收录；
        // 元音双写（ee/oo）必须满足同音或仅擦音清浊差异（保留 to/too 与 lose/loose，阻断 meet/met、good/god、check/cheek）。
        for a in indexed.iter().flatten() {
            if a.is_proper_or_mixed
                || a.has_apostrophe
                || !a.from_scowl
                || a.is_pure_contraction_typo
                || a.log_freq < -5.2
                || a.lower.len() < 3
            {
                continue;
            }
            let bytes = a.lower.as_bytes();
            for i in 0..(bytes.len() - 1) {
                if bytes[i] == bytes[i + 1] {
                    let is_vowel_double = is_ascii_vowel(bytes[i]);
                    let mut collapsed = String::with_capacity(bytes.len() - 1);
                    collapsed.push_str(&a.lower[..i]);
                    collapsed.push_str(&a.lower[i + 1..]);
                    if let Some(&b_id) = lower_to_id.get(collapsed.as_str()) {
                        if let Some(b) = &indexed[b_id as usize] {
                            if b.log_freq >= -5.2 {
                                let phon_dist = if !a.prons.is_empty() && !b.prons.is_empty() {
                                    Some(min_pron_distance(&a.prons, &b.prons))
                                } else {
                                    None
                                };
                                if is_vowel_double {
                                    let vowel_ok = match phon_dist {
                                        Some(d) => d <= 0.36,
                                        None => b.lower == "to" || b.lower == "lose",
                                    };
                                    if !vowel_ok {
                                        continue;
                                    }
                                }
                                let phon_close = phon_dist.is_some_and(|d| d <= 1.2);
                                let penalty = if b.lower.len() <= 3 || phon_close {
                                    1.25
                                } else {
                                    1.45
                                };
                                record_symmetric_edge(&mut pair_best, a, b, penalty);
                            }
                        }
                    }
                }
            }
        }

        // 5b) 相邻字母换位真词对（form/from, quiet/quite, there/three, tired/tried, trail/trial, angel/angle）
        for a in indexed.iter().flatten() {
            if a.is_proper_or_mixed
                || a.has_apostrophe
                || !a.from_scowl
                || a.is_pure_contraction_typo
                || a.log_freq < -4.8
                || a.lower.len() < 4
            {
                continue;
            }
            let mut buf = a.lower.as_bytes().to_vec();
            for i in 0..(buf.len() - 1) {
                if buf[i] != buf[i + 1] {
                    buf.swap(i, i + 1);
                    if let Ok(swapped) = std::str::from_utf8(&buf) {
                        if let Some(&b_id) = lower_to_id.get(swapped) {
                            if b_id > a.word_id {
                                if let Some(b) = &indexed[b_id as usize] {
                                    if b.log_freq >= -4.8
                                        && !are_us_uk_spelling_variants(&a.lower, &b.lower)
                                    {
                                        let phon_close = !a.prons.is_empty()
                                            && !b.prons.is_empty()
                                            && min_pron_distance(&a.prons, &b.prons) <= 1.0;
                                        let penalty = if phon_close { 1.35 } else { 1.45 };
                                        record_symmetric_edge(&mut pair_best, a, b, penalty);
                                    }
                                }
                            }
                        }
                    }
                    buf.swap(i, i + 1);
                }
            }
        }

        // 5c) 词尾齿龈清浊脱落 /n/ <-> /nd/（an/and）
        for a in indexed.iter().flatten() {
            if a.is_proper_or_mixed
                || a.has_apostrophe
                || !a.from_scowl
                || a.log_freq < -3.8
                || !a.lower.ends_with('n')
            {
                continue;
            }
            let wd = format!("{}d", a.lower);
            if let Some(&b_id) = lower_to_id.get(wd.as_str()) {
                if let Some(b) = &indexed[b_id as usize] {
                    if b.log_freq >= -3.8 {
                        let matched = !has_cmudict
                            || a.prons.iter().any(|pa| {
                                b.prons.iter().any(|pb| {
                                    pb.len() == pa.len() + 1
                                        && pb[..pa.len()] == pa[..]
                                        && pb[pa.len()] == PH_D
                                })
                            });
                        if matched {
                            record_symmetric_edge(&mut pair_best, a, b, 1.30);
                        }
                    }
                }
            }
        }

        // 5d) 超高频极短直接 QWERTY 邻键真词对：
        // - 2 字母词仅允许首元音邻键互换（in<->on），阻断首辅音互换（do/so, be/he, go/to）；
        // - 3 字母词仅允许尾字母邻键互换（has<->had）并赋予保守惩罚 1.95，供强语境/L3 使用而不破坏常规 L2 句法。
        let short_top: Vec<&IndexedWord> = indexed
            .iter()
            .flatten()
            .filter(|w| {
                !w.is_proper_or_mixed
                    && !w.has_apostrophe
                    && w.from_scowl
                    && !w.is_pure_contraction_typo
                    && w.log_freq >= -2.8
                    && matches!(w.lower.len(), 2 | 3)
                    && w.lower.bytes().all(|b| b.is_ascii_alphabetic())
            })
            .collect();
        for i in 0..short_top.len() {
            for j in (i + 1)..short_top.len() {
                let a = short_top[i];
                let b = short_top[j];
                if a.lower.len() != b.lower.len() {
                    continue;
                }
                let mut diff_info = None;
                let mut diffs = 0usize;
                for (pos, (&ba, &bb)) in a
                    .lower
                    .as_bytes()
                    .iter()
                    .zip(b.lower.as_bytes().iter())
                    .enumerate()
                {
                    if ba != bb {
                        diffs += 1;
                        if diffs > 1 {
                            break;
                        }
                        diff_info = Some((pos, ba, bb));
                    }
                }
                if diffs == 1 {
                    if let Some((pos, ba, bb)) = diff_info {
                        if substitution_cost(ba, bb) <= 0.46 {
                            if a.lower.len() == 2 && pos == 0 && is_ascii_vowel(ba) && is_ascii_vowel(bb) {
                                record_symmetric_edge(&mut pair_best, a, b, 1.45);
                            } else if a.lower.len() == 3 && pos == 2 {
                                record_symmetric_edge(&mut pair_best, a, b, 1.95);
                            }
                        }
                    }
                }
            }
        }

        let mut edges_by_word = vec![Vec::new(); num_words];
        for (wid, map) in pair_best.into_iter().enumerate() {
            if map.is_empty() {
                continue;
            }
            let mut vec: Vec<ConfusableEdge> = map
                .into_iter()
                .map(|(target_id, relative_penalty)| ConfusableEdge {
                    target_id,
                    relative_penalty,
                })
                .collect();
            // 优先保留“低惩罚 + 高频”目标，避免低频生僻同音词占满槽位
            vec.sort_by(|a, b| {
                let fa = indexed[a.target_id as usize]
                    .as_ref()
                    .map(|w| w.log_freq)
                    .unwrap_or(-9.0);
                let fb = indexed[b.target_id as usize]
                    .as_ref()
                    .map(|w| w.log_freq)
                    .unwrap_or(-9.0);
                let sa = fa * 0.35 - a.relative_penalty;
                let sb = fb * 0.35 - b.relative_penalty;
                sb.total_cmp(&sa)
                    .then_with(|| a.target_id.cmp(&b.target_id))
            });
            vec.truncate(MAX_STATIC_NEIGHBORS_PER_WORD);
            edges_by_word[wid] = vec;
        }

        Self { edges_by_word }
    }

    #[inline]
    pub(crate) fn edges_for(&self, word_id: u32) -> &[ConfusableEdge] {
        self.edges_by_word
            .get(word_id as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[inline]
fn is_ascii_vowel(b: u8) -> bool {
    matches!(b.to_ascii_lowercase(), b'a' | b'e' | b'i' | b'o' | b'u')
}

fn is_eligible_entry(entry: &ConfusableSourceEntry<'_>) -> bool {
    if entry.word.len() < 2 || entry.log_freq < MIN_INDEX_LOG_FREQ {
        return false;
    }
    if !entry.lower.bytes().any(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    if entry.lower.bytes().any(|b| b == b' ' || b == b'-') {
        return false;
    }
    // 全局过滤低频普通名词所有格 `'s`（如 king's / state's / cat's），防止通过精确同音桶污染复数名词
    if entry.lower.ends_with("'s") && entry.log_freq < MIN_APOSTROPHE_POSSESSIVE_S_LOG_FREQ {
        return false;
    }
    if entry.is_proper_or_mixed
        && !entry.lower.contains('\'')
        && entry.log_freq < MIN_PROPER_TARGET_LOG_FREQ
    {
        return false;
    }
    true
}

fn record_directed_edge(
    pair_best: &mut [HashMap<u32, f32>],
    from: &IndexedWord,
    to: &IndexedWord,
    penalty: f32,
) {
    if from.word_id == to.word_id {
        return;
    }
    if !can_serve_as_target(to) || (from.log_freq - to.log_freq) > MAX_TARGET_FREQ_DROP {
        return;
    }
    pair_best[from.word_id as usize]
        .entry(to.word_id)
        .and_modify(|p| {
            if penalty < *p {
                *p = penalty;
            }
        })
        .or_insert(penalty);
}

fn record_symmetric_edge(
    pair_best: &mut [HashMap<u32, f32>],
    a: &IndexedWord,
    b: &IndexedWord,
    penalty: f32,
) {
    record_directed_edge(pair_best, a, b, penalty);
    record_directed_edge(pair_best, b, a, penalty);
}

fn can_serve_as_target(w: &IndexedWord) -> bool {
    if w.is_pure_contraction_typo {
        return false;
    }
    if w.is_proper_or_mixed && !w.has_apostrophe && w.log_freq < MIN_PROPER_TARGET_LOG_FREQ {
        return false;
    }
    // 2~3 字母极短目标词必须具备常见词频（Zipf >= 5.2），防止 de/mi/ar/er/lat 等低频缩写成为常规词同音目标
    if !w.has_apostrophe && w.lower.len() <= 3 && w.log_freq < -3.8 {
        return false;
    }
    true
}

/// 识别美式/英式拼写变体（-or/-our, -er/-re, -ize/-ise, -og/-ogue），避免在同义异形词间无意义翻转。
fn are_us_uk_spelling_variants(a: &str, b: &str) -> bool {
    fn normalize_dialect(s: &str) -> String {
        let mut out = s.replace("our", "or");
        if out.ends_with("re") && out.len() >= 5 {
            out.truncate(out.len() - 2);
            out.push_str("er");
        } else if out.ends_with("ise") && out.len() >= 5 {
            out.truncate(out.len() - 3);
            out.push_str("ize");
        } else if out.ends_with("ogue") && out.len() >= 6 {
            out.truncate(out.len() - 4);
            out.push_str("og");
        }
        out
    }
    a != b && normalize_dialect(a) == normalize_dialect(b)
}

fn ascii_alpha_mask(s: &str) -> u32 {
    let mut mask = 0u32;
    for b in s.bytes() {
        if b.is_ascii_lowercase() {
            mask |= 1u32 << (b - b'a');
        }
    }
    mask
}

// ============================================================================
// 紧凑 ARPAbet 音素编码与加权音素编辑距离
// ============================================================================

const PH_P: u8 = 1;
const PH_B: u8 = 2;
const PH_T: u8 = 3;
const PH_D: u8 = 4;
const PH_K: u8 = 5;
const PH_G: u8 = 6;
const PH_F: u8 = 7;
const PH_V: u8 = 8;
const PH_TH: u8 = 9;
const PH_DH: u8 = 10;
const PH_S: u8 = 11;
const PH_Z: u8 = 12;
const PH_SH: u8 = 13;
const PH_ZH: u8 = 14;
const PH_CH: u8 = 15;
const PH_JH: u8 = 16;
const PH_M: u8 = 17;
const PH_N: u8 = 18;
const PH_NG: u8 = 19;
const PH_L: u8 = 20;
const PH_R: u8 = 21;
const PH_W: u8 = 22;
const PH_Y: u8 = 23;
const PH_HH: u8 = 24;
const PH_ER: u8 = 25;

// 元音编码 >= 26
const PH_AH: u8 = 26;
const PH_IH: u8 = 27;
const PH_EH: u8 = 28;
const PH_AE: u8 = 29;
const PH_AA: u8 = 30;
const PH_AO: u8 = 31;
const PH_UH: u8 = 32;
const PH_IY: u8 = 33;
const PH_EY: u8 = 34;
const PH_OW: u8 = 35;
const PH_UW: u8 = 36;
const PH_AW: u8 = 37;
const PH_AY: u8 = 38;
const PH_OY: u8 = 39;

fn parse_phoneme_code(tok: &str) -> Option<u8> {
    let clean = tok.trim_end_matches(|c: char| matches!(c, '0' | '1' | '2'));
    Some(match clean {
        "P" => PH_P,
        "B" => PH_B,
        "T" => PH_T,
        "D" => PH_D,
        "K" => PH_K,
        "G" => PH_G,
        "F" => PH_F,
        "V" => PH_V,
        "TH" => PH_TH,
        "DH" => PH_DH,
        "S" => PH_S,
        "Z" => PH_Z,
        "SH" => PH_SH,
        "ZH" => PH_ZH,
        "CH" => PH_CH,
        "JH" => PH_JH,
        "M" => PH_M,
        "N" => PH_N,
        "NG" => PH_NG,
        "L" => PH_L,
        "R" => PH_R,
        "W" => PH_W,
        "Y" => PH_Y,
        "HH" => PH_HH,
        "ER" => PH_ER,
        "AH" => PH_AH,
        "IH" => PH_IH,
        "EH" => PH_EH,
        "AE" => PH_AE,
        "AA" => PH_AA,
        "AO" => PH_AO,
        "UH" => PH_UH,
        "IY" => PH_IY,
        "EY" => PH_EY,
        "OW" => PH_OW,
        "UW" => PH_UW,
        "AW" => PH_AW,
        "AY" => PH_AY,
        "OY" => PH_OY,
        _ => return None,
    })
}

fn parse_pronunciations(raw: &str) -> Vec<Vec<u8>> {
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for part in raw.split('|') {
        let codes: Vec<u8> = part
            .split_whitespace()
            .filter_map(parse_phoneme_code)
            .collect();
        if !codes.is_empty() {
            out.push(codes);
        }
    }
    out
}

/// 将 ARPAbet 音素映射为擦音清浊合并骨架：
/// 仅合并擦音清浊对（F/V, TH/DH, S/Z, SH/ZH）及 rhotic ER->R，保持塞音/塞擦音 P/B, T/D, K/G, CH/JH 区分以防词形/时态误合（build vs built）。
fn consonant_voicing_skeleton(pron: &[u8]) -> String {
    let mut out = String::with_capacity(pron.len());
    for &ph in pron {
        let ch = match ph {
            PH_P => 'P',
            PH_B => 'B',
            PH_T => 'T',
            PH_D => 'D',
            PH_K => 'K',
            PH_G => 'G',
            PH_CH => 'C',
            PH_JH => 'J',
            PH_F | PH_V => 'F',
            PH_TH | PH_DH => 'H',
            PH_S | PH_Z => 'S',
            PH_SH | PH_ZH => 'X',
            PH_M => 'M',
            PH_N => 'N',
            PH_NG => 'Q',
            PH_L => 'L',
            PH_R | PH_ER => 'R',
            PH_W => 'W',
            PH_Y => 'Y',
            PH_HH => 'h',
            _ => continue,
        };
        out.push(ch);
    }
    out
}

#[inline]
fn is_pure_vowel(ph: u8) -> bool {
    ph >= PH_AH
}

#[inline]
fn is_rhotic_mergeable_vowel(ph: u8) -> bool {
    // 仅中/央/低元音 + R 可与 ER 合流（where/were, moral/morale）；排除高前元音 IH+R（防止 here/her 误连）
    matches!(ph, PH_EH | PH_AO | PH_AH | PH_AE | PH_AA)
}

#[inline]
fn count_syllables(pron: &[u8]) -> usize {
    pron.iter()
        .filter(|&&p| is_pure_vowel(p) || p == PH_ER)
        .count()
        .max(1)
}

#[inline]
fn has_fricative_voicing_diff(pa: &[u8], pb: &[u8]) -> bool {
    let has_voiceless = |p: &[u8]| p.iter().any(|&x| matches!(x, PH_F | PH_TH | PH_S | PH_SH));
    let has_voiced = |p: &[u8]| p.iter().any(|&x| matches!(x, PH_V | PH_DH | PH_Z | PH_ZH));
    (has_voiceless(pa) && has_voiced(pb)) || (has_voiced(pa) && has_voiceless(pb))
}

/// 在音节结构兼容的读音对之间计算最小音素编辑距离：
/// - 单音节词对（如 bug/big、rust/rest、set/sat、check/cheek）禁止仅靠重读元音替换匹配，
///   仅允许擦音清浊交替（lose/loose、breath/breathe）或超高频虚词对（then/than、where/were）；
/// - 多音节词对要求音节数一致，或仅相差 1 个弱读央元音（quiet/quite）。
fn best_compatible_pron_distance(a: &IndexedWord, b: &IndexedWord) -> Option<f32> {
    let both_top_func = a.log_freq >= -3.0 && b.log_freq >= -3.0;
    let mut best = f32::INFINITY;
    for pa in &a.prons {
        let syl_a = count_syllables(pa);
        for pb in &b.prons {
            let syl_b = count_syllables(pb);
            let max_syl = syl_a.max(syl_b);
            if max_syl == 1 {
                if !both_top_func && !has_fricative_voicing_diff(pa, pb) {
                    continue;
                }
            } else if syl_a != syl_b {
                if syl_a.abs_diff(syl_b) > 1 {
                    continue;
                }
                // 音节数差 1 时（如 quiet K W AY AH T vs quite K W AY T），仅允许弱读 AH/IH/ER 脱落（距离 <= 0.60）
                let d = phoneme_edit_distance(pa, pb);
                if d <= 0.60 && d < best {
                    best = d;
                }
                continue;
            }
            let d = phoneme_edit_distance(pa, pb);
            if d < best {
                best = d;
            }
        }
    }
    best.is_finite().then_some(best)
}

fn phoneme_indel_cost(ph: u8) -> f32 {
    match ph {
        // 弱读央元音/轻读音节元音脱落极常见（quiet/quite, history/histry）
        PH_AH | PH_IH | PH_ER => 0.55,
        PH_EH | PH_AE | PH_UH => 0.75,
        x if is_pure_vowel(x) => 0.90,
        _ => 1.15,
    }
}

fn phoneme_sub_cost(a: u8, b: u8) -> f32 {
    if a == b {
        return 0.0;
    }
    // 仅同部位擦音清浊对立辅音（advice S vs advise Z, breath TH vs breathe DH, loose vs lose）
    let fricative_voicing_pair = matches!(
        (a.min(b), a.max(b)),
        (PH_F, PH_V) | (PH_TH, PH_DH) | (PH_S, PH_Z) | (PH_SH, PH_ZH)
    );
    if fricative_voicing_pair {
        return 0.35;
    }

    // 央元音/非重读前元音弱化与中性化（accept/except, elicit/illicit, personal/personnel）
    let reduced_group_a = matches!(a, PH_AH | PH_IH | PH_EH | PH_AE);
    let reduced_group_b = matches!(b, PH_AH | PH_IH | PH_EH | PH_AE);
    if reduced_group_a && reduced_group_b {
        return 0.35;
    }

    // 同口腔区域临近元音或 rhotic R/ER 替换
    match (a.min(b), a.max(b)) {
        (PH_R, PH_ER) => 0.30,
        (PH_ER, PH_AH) | (PH_ER, PH_AO) => 0.55,
        (PH_IH, PH_IY) | (PH_EH, PH_IY) | (PH_EH, PH_EY) | (PH_AE, PH_EY) => 0.45,
        (PH_AE, PH_AA) | (PH_AA, PH_AO) | (PH_AO, PH_OW) | (PH_UH, PH_UW) => 0.45,
        (PH_AH, PH_AA) | (PH_AH, PH_AO) | (PH_AH, PH_OW) | (PH_AH, PH_UH) => 0.50,
        (x, y) if is_pure_vowel(x) && is_pure_vowel(y) => 0.90,
        _ => 1.25,
    }
}

fn phoneme_edit_distance(a: &[u8], b: &[u8]) -> f32 {
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return b.iter().map(|&p| phoneme_indel_cost(p)).sum();
    }
    if m == 0 {
        return a.iter().map(|&p| phoneme_indel_cost(p)).sum();
    }

    let stride = m + 1;
    let idx = |r: usize, c: usize| r * stride + c;
    let mut dp = vec![0.0_f32; (n + 1) * stride];

    for i in 1..=n {
        dp[idx(i, 0)] = dp[idx(i - 1, 0)] + phoneme_indel_cost(a[i - 1]);
    }
    for j in 1..=m {
        dp[idx(0, j)] = dp[idx(0, j - 1)] + phoneme_indel_cost(b[j - 1]);
    }

    for i in 1..=n {
        let pa = a[i - 1];
        for j in 1..=m {
            let pb = b[j - 1];
            let del = dp[idx(i - 1, j)] + phoneme_indel_cost(pa);
            let ins = dp[idx(i, j - 1)] + phoneme_indel_cost(pb);
            let sub = dp[idx(i - 1, j - 1)] + phoneme_sub_cost(pa, pb);
            let mut best = del.min(ins).min(sub);

            // 美式卷舌音中性化：[mid/central vowel] + R <-> ER（where/were, moral/morale）
            if i >= 2 && pb == PH_ER && a[i - 1] == PH_R && is_rhotic_mergeable_vowel(a[i - 2]) {
                best = best.min(dp[idx(i - 2, j - 1)] + 0.45);
            }
            if j >= 2 && pa == PH_ER && b[j - 1] == PH_R && is_rhotic_mergeable_vowel(b[j - 2]) {
                best = best.min(dp[idx(i - 1, j - 2)] + 0.45);
            }
            dp[idx(i, j)] = best;
        }
    }
    dp[idx(n, m)]
}

fn min_pron_distance(a_prons: &[Vec<u8>], b_prons: &[Vec<u8>]) -> f32 {
    let mut best = f32::INFINITY;
    for pa in a_prons {
        for pb in b_prons {
            let d = phoneme_edit_distance(pa, pb);
            if d < best {
                best = d;
            }
            if best == 0.0 {
                return 0.0;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_entry<'a>(
        word_id: u32,
        word: &'a str,
        zipf: f32,
        phonetic: &'a str,
    ) -> ConfusableSourceEntry<'a> {
        ConfusableSourceEntry {
            word_id,
            word,
            lower: word,
            log_freq: zipf - 9.0,
            is_proper_or_mixed: word.bytes().any(|b| b.is_ascii_uppercase()),
            from_scowl: true,
            phonetic,
        }
    }

    fn edge_targets(
        idx: &RealWordConfusableIndex,
        entries: &[ConfusableSourceEntry<'_>],
        wid: u32,
    ) -> Vec<String> {
        idx.edges_for(wid)
            .iter()
            .map(|e| entries[e.target_id as usize].word.to_string())
            .collect()
    }

    #[test]
    fn discovers_homophones_syllabic_near_homophones_and_apostrophes_from_cmudict() {
        let entries = vec![
            mk_entry(0, "right", 6.2, "R AY T"),
            mk_entry(1, "write", 5.8, "R AY T"),
            mk_entry(2, "colonel", 4.4, "K ER N AH L"),
            mk_entry(3, "kernel", 3.6, "K ER N AH L"),
            mk_entry(4, "accept", 4.9, "AE K S EH P T|AH K S EH P T"),
            mk_entry(5, "except", 5.0, "IH K S EH P T"),
            mk_entry(6, "personal", 5.2, "P ER S IH N AH L"),
            mk_entry(7, "personnel", 4.6, "P ER S AH N EH L"),
            mk_entry(8, "advice", 4.9, "AE D V AY S|AH D V AY S"),
            mk_entry(9, "advise", 4.1, "AE D V AY Z|AH D V AY Z"),
            mk_entry(10, "elicit", 3.1, "IH L IH S IH T"),
            mk_entry(11, "illicit", 3.4, "IH L IH S AH T"),
            mk_entry(12, "quiet", 4.7, "K W AY AH T"),
            mk_entry(13, "quite", 5.3, "K W AY T"),
            mk_entry(14, "breath", 4.5, "B R EH TH"),
            mk_entry(15, "breathe", 4.3, "B R IY DH"),
            mk_entry(16, "its", 6.5, "IH T S"),
            mk_entry(17, "it's", 6.3, "IH T S"),
            mk_entry(18, "lets", 5.2, "L EH T S"),
            mk_entry(19, "let's", 5.4, "L EH T S"),
            mk_entry(20, "moral", 4.4, "M AO R AH L"),
            mk_entry(21, "morale", 3.9, "M ER AE L"),
            mk_entry(22, "loose", 4.5, "L UW S"),
            mk_entry(23, "lose", 5.1, "L UW Z"),
            mk_entry(24, "cat", 5.4, "K AE T"),
            mk_entry(25, "good", 6.6, "G UH D"),
            mk_entry(26, "bat", 4.8, "B AE T"),
            mk_entry(27, "pat", 4.5, "P AE T"),
            mk_entry(28, "build", 5.1, "B IH L D"),
            mk_entry(29, "built", 5.0, "B IH L T"),
            mk_entry(30, "the", 7.73, "DH AH|DH IY"),
            mk_entry(31, "thee", 3.80, "DH IY"),
            mk_entry(32, "where", 6.0, "W EH R|HH W EH R"),
            mk_entry(33, "were", 6.3, "W ER"),
            mk_entry(34, "we're", 5.9, "W IY R|W IH R|W ER"),
            mk_entry(35, "bug", 5.0, "B AH G"),
            mk_entry(36, "big", 6.2, "B IH G"),
            mk_entry(37, "meet", 5.8, "M IY T"),
            mk_entry(38, "met", 5.6, "M EH T"),
            mk_entry(39, "here", 6.2, "HH IH R"),
            mk_entry(40, "her", 6.4, "HH ER"),
        ];

        let idx = RealWordConfusableIndex::build(&entries);

        assert!(edge_targets(&idx, &entries, 0).contains(&"write".to_string()));
        assert!(edge_targets(&idx, &entries, 2).contains(&"kernel".to_string()));
        assert!(edge_targets(&idx, &entries, 4).contains(&"except".to_string()));
        assert!(edge_targets(&idx, &entries, 6).contains(&"personnel".to_string()));
        assert!(edge_targets(&idx, &entries, 8).contains(&"advise".to_string()));
        assert!(edge_targets(&idx, &entries, 10).contains(&"illicit".to_string()));
        assert!(edge_targets(&idx, &entries, 12).contains(&"quite".to_string()));
        assert!(edge_targets(&idx, &entries, 14).contains(&"breathe".to_string()));
        assert!(edge_targets(&idx, &entries, 16).contains(&"it's".to_string()));
        assert!(edge_targets(&idx, &entries, 18).contains(&"let's".to_string()));
        assert!(edge_targets(&idx, &entries, 20).contains(&"morale".to_string()));
        assert!(edge_targets(&idx, &entries, 22).contains(&"lose".to_string()));
        assert!(edge_targets(&idx, &entries, 32).contains(&"were".to_string()));
        assert!(edge_targets(&idx, &entries, 33).contains(&"we're".to_string()));

        // 确保单音节重读元音词（bug/big、meet/met、here/her）、塞音时态词（build/built）及超大频差（the/thee）不误合并
        assert!(edge_targets(&idx, &entries, 24).is_empty());
        assert!(edge_targets(&idx, &entries, 25).is_empty());
        assert!(edge_targets(&idx, &entries, 26).is_empty());
        assert!(!edge_targets(&idx, &entries, 28).contains(&"built".to_string()));
        assert!(!edge_targets(&idx, &entries, 30).contains(&"thee".to_string()));
        assert!(!edge_targets(&idx, &entries, 35).contains(&"big".to_string()));
        assert!(!edge_targets(&idx, &entries, 37).contains(&"met".to_string()));
        assert!(!edge_targets(&idx, &entries, 39).contains(&"her".to_string()));
    }

    #[test]
    fn fallback_without_cmudict_recovers_there_their_theyre_and_where_were() {
        let entries = vec![
            mk_entry(0, "there", 6.6, ""),
            mk_entry(1, "their", 6.5, ""),
            mk_entry(2, "they're", 6.2, ""),
            mk_entry(3, "where", 6.8, ""),
            mk_entry(4, "were", 6.7, ""),
            mk_entry(5, "cat", 5.4, ""),
            mk_entry(6, "good", 6.6, ""),
        ];

        let idx = RealWordConfusableIndex::build(&entries);
        let t_there = edge_targets(&idx, &entries, 0);
        assert!(t_there.contains(&"their".to_string()), "{t_there:?}");
        assert!(t_there.contains(&"they're".to_string()), "{t_there:?}");

        let t_where = edge_targets(&idx, &entries, 3);
        assert!(t_where.contains(&"were".to_string()), "{t_where:?}");
        assert!(edge_targets(&idx, &entries, 5).is_empty());
    }

    #[test]
    fn lexicon_query_contextual_confusables_covers_static_and_typographical_pairs() {
        let lex = crate::Lexicon::from_word_zipfs(
            vec![
                ("of", 7.4),
                ("off", 6.2),
                ("an", 6.6),
                ("and", 7.5),
                ("from", 6.6),
                ("form", 5.5),
                ("in", 7.2),
                ("on", 7.0),
                ("has", 6.4),
                ("had", 6.5),
                ("where", 6.8),
                ("were", 6.7),
            ],
            Vec::new(),
        );

        let check = |input: &str, expected: &str| {
            let hits = lex.query_contextual_confusables(input, 6);
            assert!(
                hits.iter().any(|h| h.word == expected),
                "expected '{expected}' for '{input}', got {hits:?}"
            );
        };

        check("of", "off");
        check("an", "and");
        check("from", "form");
        check("in", "on");
        check("has", "had");
        check("where", "were");
    }
}
