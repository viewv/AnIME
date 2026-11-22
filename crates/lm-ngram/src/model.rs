//! 2/3-gram Katz Backoff 统计语言模型表与 `NgramLanguageModel` 实现。

use std::collections::HashMap;
use std::path::Path;

use crate::format::{
    BigramRecord, NgramData, NgramError, SPECIAL_BOS, SPECIAL_EOS, SPECIAL_UNK, TrigramRecord,
    UnigramRecord, load_animengr_file, parse_animengr_bytes,
};
use crate::user_db::UserStore;

const DEFAULT_UNK_LOG10_P: f32 = -6.5;
const DEFAULT_BACKOFF_LOG10: f32 = -0.39794; // log10(0.4)
const CONTEXT_LIFT_MIN: f32 = -2.5;
const CONTEXT_LIFT_MAX: f32 = 3.5;
pub(crate) const CONTEXT_LIFT_WEIGHT: f32 = 1.4;

#[inline]
fn pack_bigram_key(w1: u32, w2: u32) -> u64 {
    (u64::from(w1) << 32) | u64::from(w2)
}

#[derive(Debug, Clone)]
pub(crate) struct StaticNgramTable {
    vocab_to_id: HashMap<String, u32>,
    unk_id: u32,
    bos_id: u32,
    eos_id: u32,
    unigrams: Vec<(f32, f32)>,
    bigrams: Vec<(u64, f32, f32)>,
    trigrams: Vec<(u32, u32, u32, f32)>,
}

impl StaticNgramTable {
    pub(crate) fn from_data(data: NgramData) -> Self {
        let mut vocab_to_id = HashMap::with_capacity(data.vocab.len());
        for (idx, tok) in data.vocab.iter().enumerate() {
            vocab_to_id.insert(tok.to_ascii_lowercase(), idx as u32);
        }
        let unk_id = vocab_to_id.get(SPECIAL_UNK).copied().unwrap_or(u32::MAX);
        let bos_id = vocab_to_id.get(SPECIAL_BOS).copied().unwrap_or(u32::MAX);
        let eos_id = vocab_to_id.get(SPECIAL_EOS).copied().unwrap_or(u32::MAX);

        let mut unigrams = vec![(DEFAULT_UNK_LOG10_P, DEFAULT_BACKOFF_LOG10); data.vocab.len()];
        for u in data.unigrams {
            if (u.word_id as usize) < unigrams.len() {
                unigrams[u.word_id as usize] = (u.log10_p, u.backoff);
            }
        }

        let mut bigrams: Vec<(u64, f32, f32)> = data
            .bigrams
            .into_iter()
            .map(|b| (pack_bigram_key(b.w1, b.w2), b.log10_p, b.backoff))
            .collect();
        bigrams.sort_unstable_by_key(|item| item.0);
        bigrams.dedup_by_key(|item| item.0);

        let mut trigrams: Vec<(u32, u32, u32, f32)> = data
            .trigrams
            .into_iter()
            .map(|t| (t.w1, t.w2, t.w3, t.log10_p))
            .collect();
        trigrams.sort_unstable_by_key(|item| (item.0, item.1, item.2));
        trigrams.dedup_by_key(|item| (item.0, item.1, item.2));

        Self {
            vocab_to_id,
            unk_id,
            bos_id,
            eos_id,
            unigrams,
            bigrams,
            trigrams,
        }
    }

    #[inline]
    pub(crate) fn id_for(&self, token_lower: &str) -> u32 {
        if token_lower == SPECIAL_BOS {
            return self.bos_id;
        }
        if token_lower == SPECIAL_EOS {
            return self.eos_id;
        }
        if token_lower == SPECIAL_UNK {
            return self.unk_id;
        }
        self.vocab_to_id
            .get(token_lower)
            .copied()
            .unwrap_or(self.unk_id)
    }

    #[inline]
    pub(crate) fn unigram_by_id(&self, wid: u32) -> (f32, f32) {
        self.unigrams
            .get(wid as usize)
            .copied()
            .unwrap_or((DEFAULT_UNK_LOG10_P, DEFAULT_BACKOFF_LOG10))
    }

    #[inline]
    fn lookup_bigram_id(&self, id1: u32, id2: u32) -> Option<(f32, f32)> {
        if id1 == u32::MAX || id2 == u32::MAX {
            return None;
        }
        let key = pack_bigram_key(id1, id2);
        self.bigrams
            .binary_search_by_key(&key, |item| item.0)
            .ok()
            .map(|idx| (self.bigrams[idx].1, self.bigrams[idx].2))
    }

    #[inline]
    fn lookup_trigram_id(&self, id1: u32, id2: u32, id3: u32) -> Option<f32> {
        if id1 == u32::MAX || id2 == u32::MAX || id3 == u32::MAX {
            return None;
        }
        let key = (id1, id2, id3);
        self.trigrams
            .binary_search_by_key(&key, |item| (item.0, item.1, item.2))
            .ok()
            .map(|idx| self.trigrams[idx].3)
    }

    pub(crate) fn log10_unigram(&self, word_lower: &str) -> f32 {
        self.unigram_by_id(self.id_for(word_lower)).0
    }

    pub(crate) fn log10_bigram(&self, prev1_lower: Option<&str>, word_lower: &str) -> f32 {
        let Some(p1) = prev1_lower else {
            return self.log10_unigram(word_lower);
        };
        let id1 = self.id_for(p1);
        let id2 = self.id_for(word_lower);
        if let Some((lp, _)) = self.lookup_bigram_id(id1, id2) {
            return lp;
        }
        let (_, bo1) = self.unigram_by_id(id1);
        let (lp2, _) = self.unigram_by_id(id2);
        bo1 + lp2
    }

    pub(crate) fn log10_trigram(
        &self,
        prev2_lower: Option<&str>,
        prev1_lower: Option<&str>,
        word_lower: &str,
    ) -> f32 {
        let (Some(p2), Some(p1)) = (prev2_lower, prev1_lower) else {
            return self.log10_bigram(prev1_lower, word_lower);
        };
        let id1 = self.id_for(p2);
        let id2 = self.id_for(p1);
        let id3 = self.id_for(word_lower);
        if let Some(lp) = self.lookup_trigram_id(id1, id2, id3) {
            return lp;
        }
        let bi_bo = self
            .lookup_bigram_id(id1, id2)
            .map(|(_, bo)| bo)
            .unwrap_or(0.0);
        bi_bo + self.log10_bigram(Some(p1), word_lower)
    }

    /// 计算语境相对提升（PMI lift = log10 P(w | prev2, prev1) - log10 P(w)），
    /// 供单槽词级候选在具备上文时重排（无上文时为 0，保持 L1 独立词频基准稳定）。
    pub(crate) fn context_pmi_lift(
        &self,
        prev2_lower: Option<&str>,
        prev1_lower: Option<&str>,
        word_lower: &str,
    ) -> f32 {
        if prev1_lower.is_none() {
            return 0.0;
        }
        let cond = self.log10_trigram(prev2_lower, prev1_lower, word_lower);
        let base = self.log10_unigram(word_lower);
        (cond - base).clamp(CONTEXT_LIFT_MIN, CONTEXT_LIFT_MAX)
    }
}

/// 完整 L2 语言模型：可组合静态 `ANIMENGR` 2/3-gram 表与 `UserStore` 本机学习库。
#[derive(Debug)]
pub struct NgramLanguageModel {
    static_table: Option<StaticNgramTable>,
    user_store: UserStore,
}

impl Default for NgramLanguageModel {
    fn default() -> Self {
        Self::empty()
    }
}

impl NgramLanguageModel {
    pub fn empty() -> Self {
        Self {
            static_table: None,
            user_store: UserStore::in_memory(),
        }
    }

    pub fn with_user_store(user_store: UserStore) -> Self {
        Self {
            static_table: None,
            user_store,
        }
    }

    pub fn from_data(data: NgramData, user_store: UserStore) -> Self {
        Self {
            static_table: Some(StaticNgramTable::from_data(data)),
            user_store,
        }
    }

    pub fn from_animengr_bytes(bytes: &[u8], user_store: UserStore) -> Result<Self, NgramError> {
        let data = parse_animengr_bytes(bytes)?;
        Ok(Self::from_data(data, user_store))
    }

    pub fn from_animengr_file(path: &Path, user_store: UserStore) -> Result<Self, NgramError> {
        let data = load_animengr_file(path)?;
        Ok(Self::from_data(data, user_store))
    }

    /// 从内存 n-gram 频次直接构建轻量化统计表（主要供测试与端内小模型使用）。
    pub fn from_counts<S1, S2, S3>(
        unigram_counts: Vec<(S1, u32)>,
        bigram_counts: Vec<(S1, S2, u32)>,
        trigram_counts: Vec<(S1, S2, S3, u32)>,
    ) -> Self
    where
        S1: AsRef<str>,
        S2: AsRef<str>,
        S3: AsRef<str>,
    {
        let mut counts_map: HashMap<String, u32> = HashMap::new();
        for (w, c) in &unigram_counts {
            let low = w.as_ref().trim().to_ascii_lowercase();
            if !low.is_empty() {
                *counts_map.entry(low).or_insert(0) += (*c).max(1);
            }
        }
        counts_map.entry(SPECIAL_UNK.to_string()).or_insert(1);
        counts_map.entry(SPECIAL_BOS.to_string()).or_insert(1);
        counts_map.entry(SPECIAL_EOS.to_string()).or_insert(1);
        for (w1, w2, c) in &bigram_counts {
            for w in [w1.as_ref(), w2.as_ref()] {
                let low = w.trim().to_ascii_lowercase();
                if !low.is_empty() {
                    counts_map.entry(low).or_insert((*c).max(1));
                }
            }
        }
        for (w1, w2, w3, c) in &trigram_counts {
            for w in [w1.as_ref(), w2.as_ref(), w3.as_ref()] {
                let low = w.trim().to_ascii_lowercase();
                if !low.is_empty() {
                    counts_map.entry(low).or_insert((*c).max(1));
                }
            }
        }

        let mut vocab: Vec<String> = counts_map.keys().cloned().collect();
        vocab.sort();
        let word_to_id: HashMap<String, u32> = vocab
            .iter()
            .enumerate()
            .map(|(i, w)| (w.clone(), i as u32))
            .collect();

        let alpha = 0.1_f64;
        let sum_counts: f64 = counts_map.values().map(|&c| f64::from(c)).sum();
        let denom = sum_counts + alpha * (vocab.len() as f64);

        let mut unigrams = Vec::with_capacity(vocab.len());
        for (id, w) in vocab.iter().enumerate() {
            let c = f64::from(*counts_map.get(w).unwrap_or(&1));
            let lp = ((c + alpha) / denom).log10() as f32;
            unigrams.push(UnigramRecord {
                word_id: id as u32,
                log10_p: lp,
                backoff: DEFAULT_BACKOFF_LOG10,
            });
        }

        let mut bi_map: HashMap<(String, String), u32> = HashMap::new();
        let mut bigrams = Vec::with_capacity(bigram_counts.len());
        for (w1, w2, c) in &bigram_counts {
            let l1 = w1.as_ref().trim().to_ascii_lowercase();
            let l2 = w2.as_ref().trim().to_ascii_lowercase();
            if let (Some(&id1), Some(&id2)) = (word_to_id.get(&l1), word_to_id.get(&l2)) {
                let cnt = (*c).max(1);
                bi_map.insert((l1.clone(), l2), cnt);
                let c1 = f64::from((*counts_map.get(&l1).unwrap_or(&cnt)).max(cnt));
                let lp = ((f64::from(cnt) / c1).min(1.0)).log10() as f32;
                bigrams.push(BigramRecord {
                    w1: id1,
                    w2: id2,
                    log10_p: lp,
                    backoff: DEFAULT_BACKOFF_LOG10,
                });
            }
        }

        let mut trigrams = Vec::with_capacity(trigram_counts.len());
        for (w1, w2, w3, c) in &trigram_counts {
            let l1 = w1.as_ref().trim().to_ascii_lowercase();
            let l2 = w2.as_ref().trim().to_ascii_lowercase();
            let l3 = w3.as_ref().trim().to_ascii_lowercase();
            if let (Some(&id1), Some(&id2), Some(&id3)) =
                (word_to_id.get(&l1), word_to_id.get(&l2), word_to_id.get(&l3))
            {
                let cnt = (*c).max(1);
                let c12 = f64::from(bi_map.get(&(l1, l2)).copied().unwrap_or(cnt).max(cnt));
                let lp = ((f64::from(cnt) / c12).min(1.0)).log10() as f32;
                trigrams.push(TrigramRecord {
                    w1: id1,
                    w2: id2,
                    w3: id3,
                    log10_p: lp,
                });
            }
        }

        Self::from_data(
            NgramData {
                flags: 0,
                vocab,
                unigrams,
                bigrams,
                trigrams,
            },
            UserStore::in_memory(),
        )
    }

    pub fn has_static_lm(&self) -> bool {
        self.static_table.is_some()
    }

    pub fn vocab_size(&self) -> usize {
        self.static_table
            .as_ref()
            .map(|t| t.vocab_to_id.len())
            .unwrap_or(0)
    }

    pub fn bigram_count(&self) -> usize {
        self.static_table
            .as_ref()
            .map(|t| t.bigrams.len())
            .unwrap_or(0)
    }

    pub fn trigram_count(&self) -> usize {
        self.static_table
            .as_ref()
            .map(|t| t.trigrams.len())
            .unwrap_or(0)
    }

    pub fn user_store(&self) -> &UserStore {
        &self.user_store
    }

    pub fn snapshot_user_words(&self) -> Vec<(String, u32)> {
        self.user_store.snapshot_user_words()
    }

    pub fn flush_user_db(&self) {
        self.user_store.flush();
    }

    pub(crate) fn static_table(&self) -> Option<&StaticNgramTable> {
        self.static_table.as_ref()
    }

    pub(crate) fn score_with_history(
        &self,
        prev2_lower: Option<&str>,
        prev1_lower: Option<&str>,
        word: &str,
    ) -> f32 {
        let word_l = word.trim().to_ascii_lowercase();
        if word_l.is_empty() {
            return 0.0;
        }
        let static_lift = self
            .static_table
            .as_ref()
            .map(|t| CONTEXT_LIFT_WEIGHT * t.context_pmi_lift(prev2_lower, prev1_lower, &word_l))
            .unwrap_or(0.0);
        let user_bonus = if prev1_lower.is_some() {
            self.user_store.bigram_boost(prev1_lower, &word_l)
        } else {
            self.user_store.word_boost(&word_l)
        };
        static_lift + user_bonus
    }
}
