//! Top-K 二阶（3-gram）Beam Viterbi 词格整句解码器与上下文词史提取器。

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use crate::DecodedSentence;
use crate::LatticeSlot;
use crate::format::SPECIAL_BOS;
use crate::model::NgramLanguageModel;
use crate::user_db::tokenize_words;

const BEAM_WIDTH: usize = 24;
const MAX_CANDS_PER_SLOT: usize = 10;
const PER_STATE_TOP_K: usize = 2;

// 发射与转移融合权重（原因注释）：
// 1. L1 对精确词带 EXACT_BONUS=6.5，因此单槽对 `slot_max` 做归一化（LEX_LAMBDA=0.55），
//    使语境 confusable（-1.8 相对先验）折算约 -0.99；
// 2. LM_LAMBDA=0.85（LM:LEX 约 1.55）配合全量语料修正 KN 表：KN 的条件概率比旧加一平滑更尖锐，
//    同样的语境差在对数域更大，旧比值 2.27 会让 LM 压过用户实际键入（their->there、No->Know）。
//    core_v2 / noise_v1 上扫描显示比值 1.4–1.55 时整句与干净句保留同时最优，低于 1.4 真词消歧开始回落。
const LEX_LAMBDA: f32 = 0.55;
const LM_LAMBDA: f32 = 0.85;
const REL_LEX_FLOOR: f32 = -12.0;

/// 前缀束缓存每代条目上限。命中时每次按键只为（备选数 + 1）个词格各写入一两个新槽位，冷启动长句
/// 最多约 13 × 13 条，两代合计 1024 条足以覆盖最近几次按键；束内词序列为共享前缀的持久链表。
/// 代宽不宜过大：轮换时整代释放发生在按键路径上，2048 条一代实测会把单键 max 推到 15 ms 以上。
const BEAM_CACHE_GENERATION_CAPACITY: usize = 512;

/// 词格前缀束缓存。
///
/// Why：整句模式每次按键都要对默认切分与若干空格编辑备选各做一次完整解码，长句（十几个 token）
/// 单键可达 10 ms 以上，逼近 16 ms 的同步预算。束搜索在第 k 个槽位后的束只取决于上文历史、
/// 个人学习版本与前 k 个槽位的候选（词与词法分），与后续槽位和 n-best 条数无关；而相邻按键之间、
/// 同一按键的各备选之间绝大多数前缀完全相同（只有末词或编辑处之后不同）。按前缀哈希缓存每个槽位
/// 之后的束，解码从最长命中前缀续算，结果与从头解码逐位相同。
/// 满额时整代轮换而不是清空：整体清空会让紧随其后的一次按键全部冷解码，反而抬高单键最大延迟。
#[derive(Default)]
pub struct BeamPrefixCache {
    generations: Mutex<CacheGenerations>,
}

#[derive(Default)]
struct CacheGenerations {
    current: HashMap<u64, Arc<Vec<BeamState>>>,
    previous: HashMap<u64, Arc<Vec<BeamState>>>,
}

impl CacheGenerations {
    fn get(&mut self, key: u64) -> Option<Arc<Vec<BeamState>>> {
        if let Some(b) = self.current.get(&key) {
            return Some(b.clone());
        }
        let b = self.previous.get(&key)?.clone();
        self.insert(key, b.clone());
        Some(b)
    }

    fn insert(&mut self, key: u64, beam: Arc<Vec<BeamState>>) {
        if self.current.len() >= BEAM_CACHE_GENERATION_CAPACITY {
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.insert(key, beam);
    }
}

impl std::fmt::Debug for BeamPrefixCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self
            .generations
            .lock()
            .map(|g| g.current.len() + g.previous.len())
            .unwrap_or(0);
        f.debug_struct("BeamPrefixCache").field("entries", &n).finish()
    }
}

/// 束状态的已选词序列：共享前缀的持久链表，扩展与克隆都是 O(1)（缓存束与续算束共享同一前缀）。
struct WordNode {
    word: String,
    prev: Option<Arc<WordNode>>,
}

fn words_of(chain: &Option<Arc<WordNode>>) -> Vec<&str> {
    let mut out = Vec::new();
    let mut cur = chain.as_deref();
    while let Some(node) = cur {
        out.push(node.word.as_str());
        cur = node.prev.as_deref();
    }
    out.reverse();
    out
}

#[derive(Clone)]
struct BeamState {
    score: f32,
    prev2_lower: Option<String>,
    prev1_lower: Option<String>,
    words: Option<Arc<WordNode>>,
}

/// 从光标前文本提取供 2/3-gram 使用的最近两个小写词（遇句号/感叹号/问号/换行则重置到 `<bos>`）。
pub(crate) fn extract_context_history(context_before: &str) -> (Option<String>, Option<String>) {
    let trimmed_end = context_before.trim_end_matches([' ', '\t']);
    if trimmed_end.is_empty()
        || context_before.ends_with('\n')
        || context_before.ends_with('\r')
        || trimmed_end.ends_with('.')
        || trimmed_end.ends_with('!')
        || trimmed_end.ends_with('?')
    {
        return (None, Some(SPECIAL_BOS.to_string()));
    }

    let toks = tokenize_words(trimmed_end);
    match toks.len() {
        0 => (None, Some(SPECIAL_BOS.to_string())),
        1 => (
            Some(SPECIAL_BOS.to_string()),
            Some(toks[0].to_ascii_lowercase()),
        ),
        n => (
            Some(toks[n - 2].to_ascii_lowercase()),
            Some(toks[n - 1].to_ascii_lowercase()),
        ),
    }
}

pub(crate) fn decode_with_beam_viterbi(
    lm: &NgramLanguageModel,
    context_before: &str,
    slots: &[LatticeSlot],
    limit: usize,
) -> Vec<DecodedSentence> {
    if slots.is_empty() || limit == 0 {
        return Vec::new();
    }

    let (init_p2, init_p1) = extract_context_history(context_before);
    let static_table = lm.static_table();
    let user_store = lm.user_store();

    // 前缀哈希链：hashes[k] 覆盖上文历史、个人学习版本与前 k + 1 个槽位
    let mut hasher = DefaultHasher::new();
    (&init_p2, &init_p1, user_store.generation(), static_table.is_some()).hash(&mut hasher);
    let mut hashes = Vec::with_capacity(slots.len());
    for slot in slots {
        slot.raw_token.hash(&mut hasher);
        slot.candidates.len().hash(&mut hasher);
        for c in &slot.candidates {
            c.word.hash(&mut hasher);
            c.lex_score.to_bits().hash(&mut hasher);
        }
        hashes.push(hasher.finish());
    }
    let cache = lm.beam_cache();
    let (start, mut beam) = {
        let mut gens = cache.generations.lock().unwrap_or_else(|e| e.into_inner());
        match (0..slots.len()).rev().find_map(|k| gens.get(hashes[k]).map(|b| (k + 1, b))) {
            Some((k, b)) => (k, b.as_ref().clone()),
            None => (
                0,
                vec![BeamState {
                    score: 0.0,
                    prev2_lower: init_p2,
                    prev1_lower: init_p1,
                    words: None,
                }],
            ),
        }
    };
    let mut fresh: Vec<(u64, Arc<Vec<BeamState>>)> = Vec::with_capacity(slots.len() - start);

    for (slot_idx, slot) in slots.iter().enumerate().skip(start) {
        let fallback_word;
        let slot_cands: &[crate::SlotCandidate] = if slot.candidates.is_empty() {
            fallback_word = [crate::SlotCandidate {
                word: slot.raw_token.clone(),
                lex_score: -50.0,
            }];
            &fallback_word
        } else {
            let cap = slot.candidates.len().min(MAX_CANDS_PER_SLOT);
            &slot.candidates[..cap]
        };

        let slot_max_lex = slot_cands
            .iter()
            .map(|c| c.lex_score)
            .fold(f32::NEG_INFINITY, f32::max);

        let mut next_states = Vec::with_capacity(beam.len() * slot_cands.len());
        for state in &beam {
            for cand in slot_cands {
                let w_lower = cand.word.to_ascii_lowercase();
                let p1_ctx = state
                    .prev1_lower
                    .as_deref()
                    .filter(|&p| p != SPECIAL_BOS);

                let step_score = if let Some(table) = static_table {
                    let rel_lex = (cand.lex_score - slot_max_lex).max(REL_LEX_FLOOR);
                    let lm_logp = table.log10_trigram(
                        state.prev2_lower.as_deref(),
                        state.prev1_lower.as_deref(),
                        &w_lower,
                    );
                    let user_bonus = user_store.bigram_boost(p1_ctx, &w_lower);
                    LEX_LAMBDA * rel_lex + LM_LAMBDA * lm_logp + user_bonus
                } else {
                    cand.lex_score + user_store.bigram_boost(p1_ctx, &w_lower)
                };

                next_states.push(BeamState {
                    score: state.score + step_score,
                    prev2_lower: state.prev1_lower.clone(),
                    prev1_lower: Some(w_lower),
                    words: Some(Arc::new(WordNode {
                        word: cand.word.clone(),
                        prev: state.words.clone(),
                    })),
                });
            }
        }

        next_states.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                // 同分时按词序列字典序决胜（与从头解码一致）；同分罕见，只在此时展开链表
                .then_with(|| words_of(&a.words).cmp(&words_of(&b.words)))
        });

        let mut kept = Vec::with_capacity(BEAM_WIDTH);
        let mut state_counts: HashMap<(Option<String>, Option<String>), usize> =
            HashMap::with_capacity(BEAM_WIDTH);
        for st in next_states {
            let state_key = (st.prev2_lower.clone(), st.prev1_lower.clone());
            let cnt = state_counts.entry(state_key).or_insert(0);
            if *cnt >= PER_STATE_TOP_K {
                continue;
            }
            *cnt += 1;
            kept.push(st);
            if kept.len() >= BEAM_WIDTH {
                break;
            }
        }
        beam = kept;
        fresh.push((hashes[slot_idx], Arc::new(beam.clone())));
    }
    if !fresh.is_empty() {
        let mut gens = cache.generations.lock().unwrap_or_else(|e| e.into_inner());
        for (key, b) in fresh {
            gens.insert(key, b);
        }
    }

    let mut seen_texts = HashSet::with_capacity(limit * 2);
    let mut out = Vec::with_capacity(limit);
    let word_count = slots.len();

    for st in beam {
        let text = words_of(&st.words).join(" ");
        if seen_texts.insert(text.to_ascii_lowercase()) {
            out.push(DecodedSentence {
                text,
                score: st.score,
                word_count,
            });
            if out.len() >= limit {
                break;
            }
        }
    }

    out
}
