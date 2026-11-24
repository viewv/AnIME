//! L1 多路融合英文词法与纠错内核：结合 Trie、QWERTY 物理模糊束搜索、Double Metaphone 读音倒排与缩写恢复。

use std::collections::HashMap;
use std::path::Path;

use crate::confusable_index::{ConfusableCandidate, ConfusableSourceEntry, RealWordConfusableIndex};
use crate::contractions::{apply_input_casing, lookup_contraction};
use crate::format::{
    ENTRY_FLAG_FROM_SCOWL, LexiconBinaryEntry, LexiconData, LexiconError, MisspellingRecord,
    load_animelex_file, load_wordlist_file, parse_animelex_bytes, parse_wordlist_tsv,
    zipf_to_log_freq,
};
use crate::keyboard::{
    APOSTROPHE_SKIP_COST, TRANSPOSITION_COST, deletion_cost, insertion_cost, substitution_cost,
    weighted_damerau_levenshtein,
};
use crate::phonetic::PhoneticIndex;
use crate::trie::CompactTrie;
use crate::{LexiconCandidate, LexiconMatchKind, LexiconProvider, QueryOptions};

// 噪声信道融合权重常量：
// EXACT_BONUS = 6.5 保证合法低频真词（zipf >= 2.0）在精确匹配时绝不被高频近邻词误纠覆盖；
// TYPO_COMPLETION_PENALTY = 0.65 确保完整拼错词（如 appie->apple）优先于多余延伸词（apples），
// 同时不影响非终结截断错拼前缀（如 beutif->beautiful）。
const EXACT_BONUS: f32 = 6.5;
const EDIT_LAMBDA: f32 = 3.8;
const COMPLETION_BASE_PENALTY: f32 = 0.35;
const COMPLETION_CHAR_PENALTY: f32 = 0.06;
const TYPO_COMPLETION_PENALTY: f32 = 0.45;
const PHONETIC_LAMBDA: f32 = 2.3;
const PHONETIC_BONUS: f32 = 0.5;
const MAX_FUZZY_QUERY_LEN: usize = 40;
/// 闭合 token 近邻召回只针对常见词：一次编辑能"撞上"的长尾词极多，放开会把噪声塞进词格。
/// 短目标（<= 3 字母）更容易被任意编辑命中，因此门槛更高（Zipf >= 5.0 对 Zipf >= 4.0）。
const NEIGHBOR_MIN_LOG_FREQ: f32 = -5.0;
const NEIGHBOR_MIN_SHORT_LOG_FREQ: f32 = -4.0;
/// 替换只接受物理相邻或读音相近的键（查表代价上限），远键替换在快打中极少发生。
const NEIGHBOR_MAX_SUB_COST: f32 = 0.75;
const MAX_NEIGHBOR_TOKEN_LEN: usize = 16;

#[derive(Debug, Clone)]
struct WordSlot {
    word: String,
    lower: String,
    log_freq: f32,
    is_proper_or_mixed: bool,
}

#[derive(Debug, Clone, Copy)]
struct MisspellTarget {
    word_id: u32,
    weight: f32,
}

/// 完整 L1 英文词典索引：可加载 `ANIMELEX` v1 二进制或内存词表。
#[derive(Debug, Clone)]
pub struct Lexicon {
    words: Vec<WordSlot>,
    trie: CompactTrie,
    phonetic: PhoneticIndex,
    misspellings: HashMap<String, Vec<MisspellTarget>>,
    exact_by_lower: HashMap<String, u32>,
    confusables: RealWordConfusableIndex,
}

impl Lexicon {
    pub fn empty() -> Self {
        Self::from_data(LexiconData {
            flags: 0,
            words: Vec::new(),
            misspellings: Vec::new(),
        })
    }

    pub fn from_animelex_file(path: &Path) -> Result<Self, LexiconError> {
        let data = load_animelex_file(path)?;
        Ok(Self::from_data(data))
    }

    pub fn from_animelex_bytes(bytes: &[u8]) -> Result<Self, LexiconError> {
        let data = parse_animelex_bytes(bytes)?;
        Ok(Self::from_data(data))
    }

    pub fn from_tsv_str(content: &str, default_zipf: f32) -> Result<Self, LexiconError> {
        let data = parse_wordlist_tsv(content, default_zipf)?;
        Ok(Self::from_data(data))
    }

    pub fn from_wordlist_file(path: &Path, default_zipf: f32) -> Result<Self, LexiconError> {
        let data = load_wordlist_file(path, default_zipf)?;
        Ok(Self::from_data(data))
    }

    pub fn from_word_zipfs<I, S>(words: I, misspellings: Vec<MisspellingRecord>) -> Self
    where
        I: IntoIterator<Item = (S, f32)>,
        S: Into<String>,
    {
        let entries = words
            .into_iter()
            .enumerate()
            .map(|(idx, (w, zipf))| LexiconBinaryEntry {
                word: w.into(),
                zipf,
                freq_rank: (idx + 1) as u32,
                entry_flags: 0,
                phonetic: String::new(),
            })
            .collect();
        Self::from_data(LexiconData {
            flags: 0,
            words: entries,
            misspellings,
        })
    }

    pub fn from_data(mut data: LexiconData) -> Self {
        // 按词频从高到低排序，使 word_id 自身蕴含频率序，便于语音倒排桶天然优先返回高频词
        data.words.sort_by(|a, b| {
            b.zipf
                .total_cmp(&a.zipf)
                .then_with(|| a.freq_rank.cmp(&b.freq_rank))
                .then_with(|| a.word.cmp(&b.word))
        });

        let mut words: Vec<WordSlot> = Vec::with_capacity(data.words.len());
        let mut raw_phonetics: Vec<String> = Vec::with_capacity(data.words.len());
        let mut raw_from_scowl: Vec<bool> = Vec::with_capacity(data.words.len());
        let mut exact_by_lower: HashMap<String, u32> = HashMap::with_capacity(data.words.len());

        for entry in data.words {
            let trimmed = entry.word.trim();
            if trimmed.is_empty() {
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            if exact_by_lower.contains_key(&lower) {
                continue;
            }
            let word_id = words.len() as u32;
            let is_proper_or_mixed = trimmed.bytes().any(|b| b.is_ascii_uppercase());
            let from_scowl =
                entry.entry_flags == 0 || (entry.entry_flags & ENTRY_FLAG_FROM_SCOWL) != 0;
            exact_by_lower.insert(lower.clone(), word_id);
            words.push(WordSlot {
                word: trimmed.to_string(),
                lower,
                log_freq: zipf_to_log_freq(entry.zipf),
                is_proper_or_mixed,
            });
            raw_phonetics.push(entry.phonetic);
            raw_from_scowl.push(from_scowl);
        }

        let log_freqs: Vec<f32> = words.iter().map(|w| w.log_freq).collect();
        let trie = CompactTrie::build(
            words
                .iter()
                .enumerate()
                .map(|(id, w)| (w.lower.as_str(), id as u32, w.log_freq)),
            log_freqs,
        );
        let phonetic = PhoneticIndex::build(
            words
                .iter()
                .enumerate()
                .map(|(id, w)| (id as u32, w.lower.as_str())),
        );

        let mut misspellings: HashMap<String, Vec<MisspellTarget>> = HashMap::new();
        for rec in data.misspellings {
            let typo_l = rec.typo.trim().to_ascii_lowercase();
            let target_l = rec.target.trim().to_ascii_lowercase();
            if let Some(&word_id) = exact_by_lower.get(&target_l) {
                misspellings
                    .entry(typo_l)
                    .or_default()
                    .push(MisspellTarget {
                        word_id,
                        weight: rec.weight,
                    });
            }
        }

        let confusables = {
            let sources: Vec<ConfusableSourceEntry<'_>> = words
                .iter()
                .enumerate()
                .map(|(id, w)| ConfusableSourceEntry {
                    word_id: id as u32,
                    word: w.word.as_str(),
                    lower: w.lower.as_str(),
                    log_freq: w.log_freq,
                    is_proper_or_mixed: w.is_proper_or_mixed,
                    from_scowl: raw_from_scowl[id],
                    phonetic: raw_phonetics[id].as_str(),
                })
                .collect();
            RealWordConfusableIndex::build(&sources)
        };

        Self {
            words,
            trie,
            phonetic,
            misspellings,
            exact_by_lower,
            confusables,
        }
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn misspelling_count(&self) -> usize {
        self.misspellings.len()
    }

    #[inline]
    pub(crate) fn contains_exact_lower(&self, lower: &str) -> bool {
        self.exact_by_lower.contains_key(lower)
    }

    /// 主词典中该小写词形作为纯小写普通词条时的对数词频；专名 / 混写词条或未收录返回 `None`。
    #[inline]
    pub(crate) fn lowercase_canonical_log_freq(&self, lower: &str) -> Option<f32> {
        self.exact_by_lower.get(lower).and_then(|&id| {
            let slot = &self.words[id as usize];
            (!slot.is_proper_or_mixed).then_some(slot.log_freq)
        })
    }

    fn insert_candidate(
        best: &mut HashMap<String, LexiconCandidate>,
        cand: LexiconCandidate,
    ) {
        let key = cand.word.to_ascii_lowercase();
        match best.get_mut(&key) {
            Some(existing) => {
                let replace = if (cand.score - existing.score).abs() <= 1e-5 {
                    cand.kind == LexiconMatchKind::Exact && existing.kind != LexiconMatchKind::Exact
                } else {
                    cand.score > existing.score
                };
                if replace {
                    *existing = cand;
                }
            }
            None => {
                best.insert(key, cand);
            }
        }
    }

    /// 跨层复用查询入口：
    /// - `include_builtins`: 仅主基础层注入内置缩写/大小写兜底（`i->I`、`dont->don't`），避免扩展层重复注入并被加权偏移；
    /// - `suppress_heavy_fuzzy`: 当上层已存在精确命中时，主层走轻量快路径以跳过 64 束重搜索与全量语音桶扫描，守住 `< 1 ms` 预算。
    pub(crate) fn query_layer(
        &self,
        raw: &str,
        opts: &QueryOptions,
        include_builtins: bool,
        suppress_heavy_fuzzy: bool,
    ) -> Vec<LexiconCandidate> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || opts.limit == 0 || (self.words.is_empty() && !include_builtins) {
            return Vec::new();
        }
        let needle = trimmed.to_ascii_lowercase();
        let mut best: HashMap<String, LexiconCandidate> = HashMap::with_capacity(opts.limit * 4);
        let exact_hit_id = self.exact_by_lower.get(&needle).copied();
        let use_exact_fast_path = exact_hit_id.is_some() || suppress_heavy_fuzzy;

        // 1. 精确命中快路径 vs 完整模糊召回路径
        if use_exact_fast_path {
            if let Some(wid) = exact_hit_id {
                let slot = &self.words[wid as usize];
                Self::insert_candidate(
                    &mut best,
                    LexiconCandidate {
                        word: apply_input_casing(trimmed, &slot.word, slot.is_proper_or_mixed),
                        kind: LexiconMatchKind::Exact,
                        edit_cost: 0.0,
                        log_freq: slot.log_freq,
                        score: slot.log_freq + EXACT_BONUS,
                    },
                );
            }
            if let Some(node_id) = self.trie.find_node(&needle) {
                for (cid, extra_chars) in self.trie.complete_prefix(node_id, opts.limit) {
                    let cslot = &self.words[cid as usize];
                    Self::insert_candidate(
                        &mut best,
                        LexiconCandidate {
                            word: apply_input_casing(
                                trimmed,
                                &cslot.word,
                                cslot.is_proper_or_mixed,
                            ),
                            kind: LexiconMatchKind::Completion,
                            edit_cost: 0.0,
                            log_freq: cslot.log_freq,
                            score: cslot.log_freq
                                - COMPLETION_BASE_PENALTY
                                - COMPLETION_CHAR_PENALTY * (extra_chars as f32),
                        },
                    );
                }
            }
            // 对合法真词仅补充轻量单步近邻纠错（供 L2/L3 做语境真词消歧，如 form->from），跳过重型补全与语音扫描
            if needle.len() >= 3 && needle.len() <= MAX_FUZZY_QUERY_LEN && opts.max_edit_cost > 0.0
            {
                let (terminal_hits, _) =
                    self.trie
                        .fuzzy_search_with_width(&needle, opts.max_edit_cost.min(0.75), 0, 24);
                for hit in terminal_hits {
                    if hit.edit_cost == 0.0 {
                        continue;
                    }
                    let tslot = &self.words[hit.word_id as usize];
                    Self::insert_candidate(
                        &mut best,
                        LexiconCandidate {
                            word: apply_input_casing(
                                trimmed,
                                &tslot.word,
                                tslot.is_proper_or_mixed,
                            ),
                            kind: LexiconMatchKind::TypoCorrection,
                            edit_cost: hit.edit_cost,
                            log_freq: tslot.log_freq,
                            score: tslot.log_freq - EDIT_LAMBDA * hit.edit_cost,
                        },
                    );
                }
            }
        } else if needle.len() <= MAX_FUZZY_QUERY_LEN {
            // 若输入本身已是 Trie 合法前缀，收窄束宽与编辑预算兼顾精确补全与低延迟；否则全束搜索救回错拼
            let is_valid_prefix = self.trie.find_node(&needle).is_some();
            let (eff_edit, eff_beam) = if is_valid_prefix {
                (opts.max_edit_cost.min(1.35), 48)
            } else {
                (opts.max_edit_cost, 64)
            };
            let (mut terminal_hits, mut completion_hits) =
                self.trie
                    .fuzzy_search_with_width(&needle, eff_edit, 5, eff_beam);
            if opts.exhaustive_single_edit {
                // 与束搜索结果合并：同一词取较低代价由 insert_candidate 按分数去重完成
                let (t, c) = self
                    .trie
                    .single_edit_search(&needle, eff_edit, 5, needle.len() >= 5);
                terminal_hits.extend(t);
                completion_hits.extend(c);
            }

            for hit in terminal_hits {
                let slot = &self.words[hit.word_id as usize];
                let (kind, score) = if hit.edit_cost == 0.0 {
                    (LexiconMatchKind::Exact, slot.log_freq + EXACT_BONUS)
                } else {
                    (
                        LexiconMatchKind::TypoCorrection,
                        slot.log_freq - EDIT_LAMBDA * hit.edit_cost,
                    )
                };
                Self::insert_candidate(
                    &mut best,
                    LexiconCandidate {
                        word: apply_input_casing(trimmed, &slot.word, slot.is_proper_or_mixed),
                        kind,
                        edit_cost: hit.edit_cost,
                        log_freq: slot.log_freq,
                        score,
                    },
                );
            }

            for hit in completion_hits {
                let slot = &self.words[hit.word_id as usize];
                let typo_pen = if hit.edit_cost > 0.0 {
                    TYPO_COMPLETION_PENALTY
                } else {
                    0.0
                };
                let score = slot.log_freq
                    - EDIT_LAMBDA * hit.edit_cost
                    - COMPLETION_BASE_PENALTY
                    - COMPLETION_CHAR_PENALTY * (hit.extra_chars as f32)
                    - typo_pen;
                Self::insert_candidate(
                    &mut best,
                    LexiconCandidate {
                        word: apply_input_casing(trimmed, &slot.word, slot.is_proper_or_mixed),
                        kind: LexiconMatchKind::Completion,
                        edit_cost: hit.edit_cost,
                        log_freq: slot.log_freq,
                        score,
                    },
                );
            }
        } else if let Some(node_id) = self.trie.find_node(&needle) {
            if let Some(wid) = self.trie.node_word_id(node_id) {
                let slot = &self.words[wid as usize];
                Self::insert_candidate(
                    &mut best,
                    LexiconCandidate {
                        word: apply_input_casing(trimmed, &slot.word, slot.is_proper_or_mixed),
                        kind: LexiconMatchKind::Exact,
                        edit_cost: 0.0,
                        log_freq: slot.log_freq,
                        score: slot.log_freq + EXACT_BONUS,
                    },
                );
            }
            for (wid, extra_chars) in self.trie.complete_prefix(node_id, opts.limit) {
                let slot = &self.words[wid as usize];
                Self::insert_candidate(
                    &mut best,
                    LexiconCandidate {
                        word: apply_input_casing(trimmed, &slot.word, slot.is_proper_or_mixed),
                        kind: LexiconMatchKind::Completion,
                        edit_cost: 0.0,
                        log_freq: slot.log_freq,
                        score: slot.log_freq
                            - COMPLETION_BASE_PENALTY
                            - COMPLETION_CHAR_PENALTY * (extra_chars as f32),
                    },
                );
            }
        }

        // 2. 漏撇号缩写恢复与第一人称 "i" -> "I" 兜底
        if include_builtins {
            if needle == "i" && !best.contains_key("i") {
                Self::insert_candidate(
                    &mut best,
                    LexiconCandidate {
                        word: "I".to_string(),
                        kind: LexiconMatchKind::Exact,
                        edit_cost: 0.0,
                        log_freq: -1.6,
                        score: -1.6 + EXACT_BONUS,
                    },
                );
            }
            if let Some(rule) = lookup_contraction(&needle) {
                let exp_lower = rule.expanded.to_ascii_lowercase();
                let log_freq = self
                    .exact_by_lower
                    .get(&exp_lower)
                    .map(|&id| self.words[id as usize].log_freq)
                    .unwrap_or(rule.assumed_log_freq);
                let boost = if rule.collides_with_word { 2.0 } else { 5.8 };
                Self::insert_candidate(
                    &mut best,
                    LexiconCandidate {
                        word: rule.expanded.to_string(),
                        kind: LexiconMatchKind::TypoCorrection,
                        edit_cost: APOSTROPHE_SKIP_COST,
                        log_freq,
                        score: log_freq + boost,
                    },
                );
            }
        }

        // 3. 训练期常见人类错拼直达表（与评测集严格隔离）
        if let Some(targets) = self.misspellings.get(&needle) {
            for target in targets {
                let slot = &self.words[target.word_id as usize];
                let dl = weighted_damerau_levenshtein(&needle, &slot.lower).max(0.25);
                let score = slot.log_freq - 2.6 * dl + 2.0 * target.weight;
                Self::insert_candidate(
                    &mut best,
                    LexiconCandidate {
                        word: apply_input_casing(trimmed, &slot.word, slot.is_proper_or_mixed),
                        kind: LexiconMatchKind::TypoCorrection,
                        edit_cost: dl,
                        log_freq: slot.log_freq,
                        score,
                    },
                );
            }
        }

        // 4. Double Metaphone 读音倒排纠错（拯救重度哑字母/音近拼错；已精确匹配时仅保留轻量纠错不扫描重度语音桶）
        if !use_exact_fast_path
            && opts.enable_phonetic
            && (3..=MAX_FUZZY_QUERY_LEN).contains(&needle.len())
        {
            let max_len_delta: isize = if needle.len() >= 8 { 5 } else { 4 };
            for (word_id, tier) in self.phonetic.lookup(&needle, 24) {
                let slot = &self.words[word_id as usize];
                if slot.lower == needle {
                    continue;
                }
                let len_delta = (slot.lower.len() as isize - needle.len() as isize).abs();
                if len_delta > max_len_delta {
                    continue;
                }
                let dl = weighted_damerau_levenshtein(&needle, &slot.lower);
                if dl > 4.2 {
                    continue;
                }
                let tier_penalty = match tier {
                    0 => 0.0,
                    1 => 0.9,
                    _ => 1.5,
                };
                let score = slot.log_freq - PHONETIC_LAMBDA * dl - tier_penalty + PHONETIC_BONUS;
                Self::insert_candidate(
                    &mut best,
                    LexiconCandidate {
                        word: apply_input_casing(trimmed, &slot.word, slot.is_proper_or_mixed),
                        kind: LexiconMatchKind::PhoneticCorrection,
                        edit_cost: dl,
                        log_freq: slot.log_freq,
                        score,
                    },
                );
            }
        }

        let mut out: Vec<LexiconCandidate> = best.into_values().collect();
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.edit_cost.total_cmp(&b.edit_cost))
                .then_with(|| a.word.cmp(&b.word))
        });
        out.truncate(opts.limit);
        out
    }

    /// O(1) 离线数据驱动上下文易混真词检索：直接查询构建期固化的同音/近音/撇号缩写与拼写近邻静态图。
    pub fn query_contextual_confusables(
        &self,
        raw: &str,
        limit: usize,
    ) -> Vec<ConfusableCandidate> {
        let needle = raw.trim().to_ascii_lowercase();
        if needle.len() < 2 || limit == 0 {
            return Vec::new();
        }
        let Some(&base_id) = self.exact_by_lower.get(&needle) else {
            return Vec::new();
        };

        let base_log_freq = self.words[base_id as usize].log_freq;
        let mut out: Vec<ConfusableCandidate> = self
            .confusables
            .edges_for(base_id)
            .iter()
            .filter(|edge| edge.target_id != base_id)
            .map(|edge| {
                let slot = &self.words[edge.target_id as usize];
                ConfusableCandidate {
                    word: slot.word.clone(),
                    log_freq: slot.log_freq,
                    relative_penalty: edge.relative_penalty,
                }
            })
            .collect();

        out.sort_by(|a, b| {
            let gap_a = (base_log_freq - a.log_freq).clamp(-0.2, 2.5) * 0.25;
            let gap_b = (base_log_freq - b.log_freq).clamp(-0.2, 2.5) * 0.25;
            let sa = -(a.relative_penalty + gap_a);
            let sb = -(b.relative_penalty + gap_b);
            sb.total_cmp(&sa)
                .then_with(|| b.log_freq.total_cmp(&a.log_freq))
                .then_with(|| a.word.cmp(&b.word))
        });
        out.truncate(limit);
        out
    }

    /// 对未精确命中词典的疑似漏空格粘连词（如 `Ilikethe`、`thatlikes`、`dontknow`、`witha`）
    /// 执行 $O(L^2)$ 词频动态规划切分（最多 4 段，耗时 < 10 us），返回切分后的规范词序列。
    /// 已被空格闭合的 token 的一次编辑近邻（整句模式专用召回入口）。
    ///
    /// 与 Trie 模糊束搜索互补：束搜索为"正在输入的前缀"设计，短词编辑预算极低且禁止首字母漏敲，
    /// 以免刚敲一个 `o` 就弹出纠错；闭合后的词已有上下文与 L2 把关，可以放开召回：
    /// 首位漏敲（`o -> to`、`he -> the`、`hat -> that`）、多敲、QWERTY 邻键替换与相邻换位。
    /// 直接枚举编辑串查哈希（约 60×len 次），不走 Trie，代价沿用 `keyboard.rs` 物理距离模型。
    pub fn closed_token_neighbors(&self, raw: &str, limit: usize) -> Vec<LexiconCandidate> {
        let lower = raw.trim().to_ascii_lowercase();
        let n = lower.len();
        if n == 0
            || n > MAX_NEIGHBOR_TOKEN_LEN
            || limit == 0
            || !lower.bytes().all(|b| b.is_ascii_lowercase() || b == b'\'')
        {
            return Vec::new();
        }
        let b = lower.as_bytes();
        let mut best: HashMap<u32, f32> = HashMap::new();
        let mut consider = |cand: &[u8], cost: f32| {
            let Ok(s) = std::str::from_utf8(cand) else { return };
            if s == lower {
                return;
            }
            let Some(&id) = self.exact_by_lower.get(s) else { return };
            let slot = &self.words[id as usize];
            let min_lf = if s.len() <= 3 {
                NEIGHBOR_MIN_SHORT_LOG_FREQ
            } else {
                NEIGHBOR_MIN_LOG_FREQ
            };
            if slot.log_freq < min_lf || (slot.is_proper_or_mixed && slot.word != "I") {
                return;
            }
            let e = best.entry(id).or_insert(f32::INFINITY);
            if cost < *e {
                *e = cost;
            }
        };
        let mut buf: Vec<u8> = Vec::with_capacity(n + 1);
        // 用户漏敲了目标词中的一个字符（含首字母）
        for p in 0..=n {
            for c in b'a'..=b'z' {
                buf.clear();
                buf.extend_from_slice(&b[..p]);
                buf.push(c);
                buf.extend_from_slice(&b[p..]);
                consider(&buf, deletion_cost(c, p.checked_sub(1).map(|i| b[i])));
            }
        }
        // 用户多敲了一个字符
        if n >= 2 {
            for p in 0..n {
                buf.clear();
                buf.extend_from_slice(&b[..p]);
                buf.extend_from_slice(&b[p + 1..]);
                let prev = p.checked_sub(1).map(|i| b[i]);
                consider(&buf, insertion_cost(b[p], prev, b.get(p + 1).copied()));
            }
        }
        // 邻键替换与相邻换位
        for p in 0..n {
            for c in b'a'..=b'z' {
                if c == b[p] {
                    continue;
                }
                let cost = substitution_cost(b[p], c);
                if cost <= NEIGHBOR_MAX_SUB_COST {
                    buf.clear();
                    buf.extend_from_slice(b);
                    buf[p] = c;
                    consider(&buf, cost);
                }
            }
            if p + 1 < n && b[p] != b[p + 1] {
                buf.clear();
                buf.extend_from_slice(b);
                buf.swap(p, p + 1);
                consider(&buf, TRANSPOSITION_COST);
            }
        }
        let mut out: Vec<LexiconCandidate> = best
            .into_iter()
            .map(|(id, cost)| {
                let slot = &self.words[id as usize];
                LexiconCandidate {
                    word: apply_input_casing(raw.trim(), &slot.word, slot.is_proper_or_mixed),
                    kind: LexiconMatchKind::TypoCorrection,
                    edit_cost: cost,
                    log_freq: slot.log_freq,
                    score: slot.log_freq - EDIT_LAMBDA * cost,
                }
            })
            .collect();
        out.sort_by(|x, y| y.score.total_cmp(&x.score).then_with(|| x.word.cmp(&y.word)));
        out.truncate(limit);
        out
    }

    pub fn segment_runon_token(&self, raw: &str) -> Option<Vec<String>> {
        const MAX_RUNON_LEN: usize = 32;
        const MAX_SEGMENTS: usize = 4;
        const SPLIT_BOUNDARY_PENALTY: f32 = 1.75;
        const CAMEL_BOUNDARY_RELIEF: f32 = 1.05;
        const MIN_AVG_LOG_FREQ: f32 = -3.65;
        const PRONOUN_FOLLOW_VERBS: &[&str] = &[
            "like", "love", "want", "need", "think", "know", "have", "had", "am", "was", "will",
            "would", "can", "could", "should", "dont", "cant", "wont", "didnt", "hope", "wish",
            "feel", "see", "saw", "believe", "agree", "guess",
        ];
        const ARTICLE_PRECEDING_WORDS: &[&str] = &[
            "with", "in", "on", "for", "to", "of", "is", "was", "as", "have", "had", "has", "want",
            "need", "like", "take", "make", "get", "got", "give", "find", "such", "what", "not",
        ];

        let trimmed = raw.trim();
        let n = trimmed.len();
        if !(3..=MAX_RUNON_LEN).contains(&n) {
            return None;
        }
        if !trimmed.bytes().all(|b| b.is_ascii_alphabetic() || b == b'\'') {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();
        // 已精确存在于主词典或属于单个无歧义缩写时绝不拆分（保护 software / something / without / dont）
        if self.exact_by_lower.contains_key(&lower) || lookup_contraction(&lower).is_some() {
            return None;
        }

        let raw_bytes = trimmed.as_bytes();
        let lower_bytes = lower.as_bytes();

        #[derive(Clone)]
        struct DpState {
            score: f32,
            sum_log_freq: f32,
            words: Vec<String>,
            last_lower: String,
        }

        // dp[pos][k] 表示前 pos 字节切分为 k 段（1 <= k <= MAX_SEGMENTS）的最优状态
        let mut dp: Vec<Vec<Option<DpState>>> = vec![vec![None; MAX_SEGMENTS + 1]; n + 1];
        dp[0][0] = Some(DpState {
            score: 0.0,
            sum_log_freq: 0.0,
            words: Vec::new(),
            last_lower: String::new(),
        });

        for i in 0..n {
            for k in 0..MAX_SEGMENTS {
                let Some(cur) = dp[i][k].clone() else {
                    continue;
                };
                let max_j = (i + 16).min(n);
                for j in (i + 1)..=max_j {
                    let seg_len = j - i;
                    let sub_lower = &lower[i..j];
                    let sub_raw = &trimmed[i..j];

                    let (cand_word, cand_log_freq) = if seg_len == 1 {
                        let ch = lower_bytes[i];
                        if ch == b'i' && i == 0 {
                            let rest = &lower[1..];
                            let explicit_upper_i =
                                raw_bytes[0] == b'I' && raw_bytes.get(1).is_some_and(|b| b.is_ascii_lowercase());
                            let followed_by_verb = PRONOUN_FOLLOW_VERBS
                                .iter()
                                .any(|&v| rest.starts_with(v));
                            if !explicit_upper_i && !followed_by_verb {
                                continue;
                            }
                            ("I".to_string(), -1.50_f32)
                        } else if ch == b'a' {
                            let explicit_upper_a = i > 0 && raw_bytes[i] == b'A';
                            let after_prep = i > 0
                                && ARTICLE_PRECEDING_WORDS
                                    .iter()
                                    .any(|&w| cur.last_lower == w);
                            if !explicit_upper_a && !after_prep {
                                continue;
                            }
                            ("a".to_string(), -1.64_f32)
                        } else {
                            continue;
                        }
                    } else if seg_len == 2 {
                        if sub_lower == "im" && i == 0 {
                            ("I'm".to_string(), -2.30_f32)
                        } else if let Some(&wid) = self.exact_by_lower.get(sub_lower) {
                            let slot = &self.words[wid as usize];
                            if slot.is_proper_or_mixed || slot.log_freq < -3.15 {
                                continue;
                            }
                            (slot.word.clone(), slot.log_freq)
                        } else {
                            continue;
                        }
                    } else if let Some(rule) =
                        lookup_contraction(sub_lower).filter(|r| !r.collides_with_word)
                    {
                        (rule.expanded.to_string(), rule.assumed_log_freq)
                    } else if let Some(&wid) = self.exact_by_lower.get(sub_lower) {
                        let slot = &self.words[wid as usize];
                        let min_freq = if k >= 2 { -4.35 } else { -4.65 };
                        if slot.log_freq < min_freq {
                            continue;
                        }
                        if slot.is_proper_or_mixed
                            && !slot.lower.contains('\'')
                            && slot.log_freq < -3.8
                            && !sub_raw.bytes().next().is_some_and(|b| b.is_ascii_uppercase())
                        {
                            continue;
                        }
                        (slot.word.clone(), slot.log_freq)
                    } else {
                        continue;
                    };

                    let is_camel_boundary = (i > 0 && raw_bytes[i].is_ascii_uppercase())
                        || (i == 1 && raw_bytes[0] == b'I' && raw_bytes[1].is_ascii_lowercase());
                    let boundary_cost = if i == 0 {
                        0.0
                    } else if is_camel_boundary {
                        SPLIT_BOUNDARY_PENALTY - CAMEL_BOUNDARY_RELIEF
                    } else {
                        SPLIT_BOUNDARY_PENALTY
                    };

                    let next_score = cur.score + cand_log_freq - boundary_cost;
                    let next_sum = cur.sum_log_freq + cand_log_freq;
                    let should_update = match &dp[j][k + 1] {
                        Some(existing) => next_score > existing.score,
                        None => true,
                    };
                    if should_update {
                        let mut words = cur.words.clone();
                        let formatted = if i == 0 {
                            apply_input_casing(
                                sub_raw,
                                &cand_word,
                                cand_word.bytes().any(|b| b.is_ascii_uppercase()),
                            )
                        } else {
                            cand_word
                        };
                        words.push(formatted);
                        dp[j][k + 1] = Some(DpState {
                            score: next_score,
                            sum_log_freq: next_sum,
                            words,
                            last_lower: sub_lower.to_string(),
                        });
                    }
                }
            }
        }

        let mut best: Option<DpState> = None;
        for k in 2..=MAX_SEGMENTS {
            if let Some(st) = &dp[n][k] {
                let avg_freq = st.sum_log_freq / (k as f32);
                if avg_freq < MIN_AVG_LOG_FREQ {
                    continue;
                }
                if best.as_ref().is_none_or(|b| st.score > b.score) {
                    best = Some(st.clone());
                }
            }
        }
        if let Some(st) = best {
            return Some(st.words);
        }

        // 2-segment 容错连打切分回退（Typo-Tolerant Run-on Segmentation）：
        // 当精确子词 DP 未命中（例如 "takeyearys" 中左半段 "take" 是精确高频词，右半段 "yearys" 是 "years" 的拼错，
        // 或左半段带 1 键邻键拼错、右半段是精确高频词），枚举切分点 p \in 2..=(n-3)：
        // 要求一侧为精确高频词（log_freq >= -3.35），另一侧在 Trie 模糊搜索中存在低编辑代价高频纠错（edit_cost <= 0.90, log_freq >= -3.80）。
        if (6..=20).contains(&n) {
            let mut best_fuzzy: Option<(f32, Vec<String>)> = None;
            for p in 2..=(n - 2) {
                let left_lower = &lower[..p];
                let right_lower = &lower[p..];
                let left_raw = &trimmed[..p];
                let right_raw = &trimmed[p..];

                // Case A: 左半段精确命中高频词（如 "take" / "it" / "that"），右半段带单次拼写错误（如 "yearys" -> "years"）
                if right_lower.len() >= 3 {
                    let left_exact: Option<(String, f32)> = if p == 2 && left_lower == "im" {
                        Some(("I'm".to_string(), -2.30))
                    } else if let Some(rule) =
                        lookup_contraction(left_lower).filter(|r| !r.collides_with_word)
                    {
                        Some((rule.expanded.to_string(), rule.assumed_log_freq))
                    } else if let Some(&wid) = self.exact_by_lower.get(left_lower) {
                        let slot = &self.words[wid as usize];
                        let min_left = if p == 2 { -2.85 } else { -3.45 };
                        if !slot.is_proper_or_mixed && slot.log_freq >= min_left {
                            Some((slot.word.clone(), slot.log_freq))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some((l_word, l_freq)) = left_exact {
                        if let Some((r_word, r_freq, r_cost)) =
                            self.best_single_typo_subword(right_lower, right_raw)
                        {
                            let avg_freq = (l_freq + r_freq) * 0.5;
                            let score = l_freq + r_freq - EDIT_LAMBDA * r_cost - SPLIT_BOUNDARY_PENALTY;
                            if avg_freq >= -3.45
                                && best_fuzzy.as_ref().is_none_or(|(bs, _)| score > *bs)
                            {
                                let l_fmt = apply_input_casing(
                                    left_raw,
                                    &l_word,
                                    l_word.bytes().any(|b| b.is_ascii_uppercase()),
                                );
                                best_fuzzy = Some((score, vec![l_fmt, r_word]));
                            }
                        }
                    }
                }

                // Case B: 右半段精确命中高频词（如 "would" / "the" / "years"），左半段带单次拼写错误（如 "takr" -> "take"）
                if left_lower.len() >= 3 && right_lower.len() >= 2 {
                    let right_exact: Option<(String, f32)> =
                        if let Some(&wid) = self.exact_by_lower.get(right_lower) {
                            let slot = &self.words[wid as usize];
                            let min_right = if right_lower.len() == 2 { -2.85 } else { -3.45 };
                            if !slot.is_proper_or_mixed && slot.log_freq >= min_right {
                                Some((slot.word.clone(), slot.log_freq))
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                    if let Some((r_word, r_freq)) = right_exact {
                        if let Some((l_word, l_freq, l_cost)) =
                            self.best_single_typo_subword(left_lower, left_raw)
                        {
                            let avg_freq = (l_freq + r_freq) * 0.5;
                            let score = l_freq + r_freq - EDIT_LAMBDA * l_cost - SPLIT_BOUNDARY_PENALTY;
                            if avg_freq >= -3.45
                                && best_fuzzy.as_ref().is_none_or(|(bs, _)| score > *bs)
                            {
                                best_fuzzy = Some((score, vec![l_word, r_word]));
                            }
                        }
                    }
                }
            }
            if let Some((_, words)) = best_fuzzy {
                return Some(words);
            }
        }
        None
    }

    /// 供连打容错切分使用的子词级轻量模糊检索：在编辑代价 `<= 0.90`（1 次邻键替换或单字母多敲/漏敲/转置）内
    /// 寻找最高频的合法普通词（`log_freq >= -3.65`）。
    fn best_single_typo_subword(
        &self,
        sub_lower: &str,
        sub_raw: &str,
    ) -> Option<(String, f32, f32)> {
        if !(3..=12).contains(&sub_lower.len()) {
            return None;
        }
        let mut best: Option<(String, f32, f32, f32)> = None;
        if let Some(records) = self.misspellings.get(sub_lower) {
            for rec in records {
                let slot = &self.words[rec.word_id as usize];
                if !slot.is_proper_or_mixed && slot.log_freq >= -3.65 {
                    let cost = 0.65_f32;
                    let rank = slot.log_freq - EDIT_LAMBDA * cost;
                    if best.as_ref().is_none_or(|b| rank > b.3) {
                        let formatted =
                            apply_input_casing(sub_raw, &slot.word, slot.is_proper_or_mixed);
                        best = Some((formatted, slot.log_freq, cost, rank));
                    }
                }
            }
        }
        let (hits, _) = self.trie.fuzzy_search_with_width(sub_lower, 0.90, 0, 24);
        for hit in hits {
            if hit.edit_cost == 0.0 || hit.edit_cost > 0.90 {
                continue;
            }
            let slot = &self.words[hit.word_id as usize];
            if slot.is_proper_or_mixed || slot.log_freq < -3.65 {
                continue;
            }
            let rank = slot.log_freq - EDIT_LAMBDA * hit.edit_cost;
            if best.as_ref().is_none_or(|b| rank > b.3) {
                let formatted = apply_input_casing(sub_raw, &slot.word, slot.is_proper_or_mixed);
                best = Some((formatted, slot.log_freq, hit.edit_cost, rank));
            }
        }
        best.map(|(w, freq, cost, _)| (w, freq, cost))
    }
}

impl LexiconProvider for Lexicon {
    fn query(&self, raw: &str, opts: &QueryOptions) -> Vec<LexiconCandidate> {
        self.query_layer(raw, opts, true, false)
    }

    fn contextual_confusables(&self, raw: &str, limit: usize) -> Vec<ConfusableCandidate> {
        self.query_contextual_confusables(raw, limit)
    }

    fn segment_runon(&self, raw: &str) -> Option<Vec<String>> {
        self.segment_runon_token(raw)
    }

    fn closed_token_neighbors(&self, raw: &str, limit: usize) -> Vec<LexiconCandidate> {
        Lexicon::closed_token_neighbors(self, raw, limit)
    }
}
