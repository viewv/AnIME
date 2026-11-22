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
use crate::keyboard::{APOSTROPHE_SKIP_COST, weighted_damerau_levenshtein};
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

    /// 若主词典已收录该词且规范词形为纯小写（普通词汇，非专有名词/缩写），返回 `true`。
    #[inline]
    pub(crate) fn has_lowercase_canonical(&self, lower: &str) -> bool {
        self.exact_by_lower
            .get(lower)
            .is_some_and(|&id| !self.words[id as usize].is_proper_or_mixed)
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
            let (terminal_hits, completion_hits) =
                self.trie
                    .fuzzy_search_with_width(&needle, eff_edit, 5, eff_beam);

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
}

impl LexiconProvider for Lexicon {
    fn query(&self, raw: &str, opts: &QueryOptions) -> Vec<LexiconCandidate> {
        self.query_layer(raw, opts, true, false)
    }

    fn contextual_confusables(&self, raw: &str, limit: usize) -> Vec<ConfusableCandidate> {
        self.query_contextual_confusables(raw, limit)
    }
}
