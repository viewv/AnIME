//! 平台无关英文输入法会话状态机：管理预编辑区、多路候选融合、句首大写、易混词与上下文重排、
//! 数字/部分选词、智能空格/标点回删、`seq`/`revision` 抗过期控制与 L3 异步结果合并。

mod confusables;

use std::collections::HashMap;

use anime_lexicon::{LexiconCandidate, LexiconMatchKind, LexiconProvider, QueryOptions};
use anime_lm_neural::{NeuralCandidate, NeuralSentenceModel};
use anime_lm_ngram::{LatticeSlot, NgramModel, SlotCandidate};
use anime_protocol::{
    Candidate, CandidateKind, CommitAction, ContextText, EngineConfig, KeyInput, PreeditState,
};

use crate::confusables::{expand_closed_token_neighbors, expand_slot_confusables};

const MAX_CONTEXT_BEFORE_CHARS: usize = 256;
const MAX_TOKEN_LEX_CACHE: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum EngineOutput {
    State(PreeditState),
    Commit(CommitAction),
}

pub struct EngineSession<L, N, M> {
    lexicon: L,
    ngram: N,
    neural: M,
    config: EngineConfig,
    context: ContextText,
    privacy: bool,
    preedit: String,
    candidates: Vec<Candidate>,
    seq: u64,
    revision: u32,
    last_auto_space: bool,
    // 在单次预编辑组合内按完整词槽缓存 L1 结果：多词输入时前序未变词槽无需每击重算 400 us 模糊束搜索，
    // 兼顾退格回退命中，并在配置变更、重置或提交选词后立即清空。
    token_lex_cache: HashMap<String, Vec<LexiconCandidate>>,
    neighbor_cache: HashMap<String, Vec<LexiconCandidate>>,
}

impl<L, N, M> EngineSession<L, N, M>
where
    L: LexiconProvider,
    N: NgramModel,
    M: NeuralSentenceModel,
{
    pub fn new(lexicon: L, ngram: N, neural: M) -> Self {
        Self {
            lexicon,
            ngram,
            neural,
            config: EngineConfig::default(),
            context: ContextText::default(),
            privacy: false,
            preedit: String::new(),
            candidates: Vec::new(),
            seq: 0,
            revision: 0,
            last_auto_space: false,
            token_lex_cache: HashMap::with_capacity(16),
            neighbor_cache: HashMap::with_capacity(16),
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: EngineConfig) {
        self.config = config;
        self.revision = 0;
        self.token_lex_cache.clear();
        self.recompute_candidates();
    }

    pub fn set_context(&mut self, before: impl Into<String>, after: impl Into<String>) {
        self.context = ContextText {
            before: before.into(),
            after: after.into(),
        };
        if !self.context.before.ends_with(' ') {
            self.last_auto_space = false;
        }
        self.revision = 0;
        self.recompute_candidates();
    }

    pub fn context(&self) -> &ContextText {
        &self.context
    }

    pub fn set_privacy(&mut self, enabled: bool) {
        self.privacy = enabled;
    }

    pub fn is_privacy(&self) -> bool {
        self.privacy
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn revision(&self) -> u32 {
        self.revision
    }

    pub fn lexicon(&self) -> &L {
        &self.lexicon
    }

    pub fn ngram(&self) -> &N {
        &self.ngram
    }

    pub fn neural(&self) -> &M {
        &self.neural
    }

    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    pub fn neural_backend(&self) -> &'static str {
        self.neural.backend_name()
    }

    pub fn reset(&mut self) -> PreeditState {
        let next_seq = self.seq.saturating_add(1);
        self.reset_with_seq(next_seq)
    }

    pub fn reset_with_seq(&mut self, seq: u64) -> PreeditState {
        self.seq = seq;
        self.revision = 0;
        self.preedit.clear();
        self.candidates.clear();
        self.token_lex_cache.clear();
        self.last_auto_space = false;
        self.current_state(false)
    }

    pub fn handle_key(&mut self, key: KeyInput) -> EngineOutput {
        let next_seq = self.seq.saturating_add(1);
        self.handle_key_with_seq(next_seq, key)
    }

    pub fn handle_key_with_seq(&mut self, seq: u64, key: KeyInput) -> EngineOutput {
        self.seq = seq;
        self.revision = 0;
        match key {
            KeyInput::Char(ch) => self.handle_char(ch),
            KeyInput::Space => self.handle_space(),
            KeyInput::Backspace => {
                if self.preedit.is_empty() {
                    self.last_auto_space = false;
                    EngineOutput::State(self.current_state(true))
                } else {
                    self.preedit.pop();
                    self.last_auto_space = false;
                    self.recompute_candidates();
                    EngineOutput::State(self.current_state(false))
                }
            }
            KeyInput::Enter => {
                if self.preedit.is_empty() {
                    self.last_auto_space = false;
                    EngineOutput::State(self.current_state(true))
                } else {
                    let raw = std::mem::take(&mut self.preedit);
                    self.candidates.clear();
                    self.token_lex_cache.clear();
                    self.last_auto_space = false;
                    self.ngram
                        .learn_commit(&self.context.before, &raw, self.privacy);
                    append_context_before(&mut self.context.before, &raw, false);
                    EngineOutput::Commit(CommitAction {
                        seq: self.seq,
                        text: raw,
                        delete_before: 0,
                        remaining_preedit: String::new(),
                        candidates: Vec::new(),
                    })
                }
            }
            KeyInput::Escape => {
                if self.preedit.is_empty() {
                    EngineOutput::State(self.current_state(true))
                } else {
                    self.preedit.clear();
                    self.candidates.clear();
                    self.token_lex_cache.clear();
                    self.last_auto_space = false;
                    EngineOutput::State(self.current_state(false))
                }
            }
            KeyInput::Digit(d) => {
                if self.preedit.is_empty() {
                    self.last_auto_space = false;
                    if d <= 9 {
                        let mut ch_buf = [0u8; 4];
                        let ch = char::from(b'0' + d);
                        append_context_before(
                            &mut self.context.before,
                            ch.encode_utf8(&mut ch_buf),
                            false,
                        );
                    }
                    EngineOutput::State(self.current_state(true))
                } else if (1..=9).contains(&d) {
                    let idx = usize::from(d - 1);
                    if idx < self.candidates.len() {
                        self.commit_candidate_at(idx)
                    } else {
                        EngineOutput::State(self.current_state(false))
                    }
                } else {
                    EngineOutput::State(self.current_state(false))
                }
            }
        }
    }

    pub fn select(&mut self, index: usize) -> Option<CommitAction> {
        let next_seq = self.seq.saturating_add(1);
        self.select_with_seq(next_seq, index)
    }

    pub fn select_with_seq(&mut self, seq: u64, index: usize) -> Option<CommitAction> {
        self.seq = seq;
        self.revision = 0;
        if self.preedit.is_empty() || index >= self.candidates.len() {
            return None;
        }
        match self.commit_candidate_at(index) {
            EngineOutput::Commit(action) => Some(action),
            EngineOutput::State(_) => None,
        }
    }

    pub fn snapshot_state(&self, passthrough: bool, final_l3: bool) -> PreeditState {
        PreeditState {
            seq: self.seq,
            revision: self.revision,
            cursor: self.preedit.chars().count(),
            preedit: self.preedit.clone(),
            candidates: self.candidates.clone(),
            passthrough,
            final_l3,
        }
    }

    /// 合并异步 L3 神经重排与续写预测：若 `expected_seq != self.seq` 则严格丢弃过期结果。
    pub fn apply_neural_rerank(
        &mut self,
        expected_seq: u64,
        reranked: &[NeuralCandidate],
        continuations: &[NeuralCandidate],
    ) -> Option<PreeditState> {
        if expected_seq != self.seq || self.preedit.is_empty() {
            return None;
        }

        self.revision = self.revision.saturating_add(1);
        let total_chars = self.preedit.chars().count();

        self.candidates
            .retain(|c| c.kind != CandidateKind::Continuation);

        let mut primary: Vec<Candidate> = Vec::with_capacity(self.candidates.len());
        let mut partial: Vec<Candidate> = Vec::new();
        for mut c in self.candidates.drain(..) {
            if c.kind == CandidateKind::Raw && c.consumed_chars == total_chars {
                continue;
            }
            if c.consumed_chars < total_chars {
                partial.push(c);
                continue;
            }
            if let Some(nr) = reranked
                .iter()
                .find(|r| !r.is_continuation && r.text == c.text)
                .or_else(|| {
                    reranked
                        .iter()
                        .find(|r| !r.is_continuation && r.text.eq_ignore_ascii_case(&c.text))
                })
            {
                c.score += nr.score;
            }
            primary.push(c);
        }

        primary.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.text.cmp(&b.text)));

        let mut continuation_cands: Vec<Candidate> = Vec::new();
        for nc in continuations {
            let trimmed = nc.text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if primary
                .iter()
                .chain(continuation_cands.iter())
                .any(|c| c.text.eq_ignore_ascii_case(trimmed))
            {
                continue;
            }
            continuation_cands.push(Candidate {
                text: trimmed.to_string(),
                kind: CandidateKind::Continuation,
                comment: Some("✦".into()),
                score: nc.score,
                consumed_chars: total_chars,
                trailing_space: self.config.smart_spacing,
            });
        }
        continuation_cands.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.text.cmp(&b.text)));

        let max_total = self.config.max_candidates;
        let has_raw_in_primary = primary
            .iter()
            .any(|c| c.text == self.preedit && c.consumed_chars == total_chars);
        let slot_budget = if has_raw_in_primary {
            max_total
        } else {
            max_total.saturating_sub(1)
        };
        // 保留第 1 位主候选不变，将最多 2 条 `✦` 续写前置到第 2–3 位，确保在 5 项竖排分页模式下也能在第 1 页直接可见。
        let cont_take = if continuation_cands.is_empty() || slot_budget < 2 {
            0
        } else {
            continuation_cands
                .len()
                .min(2)
                .min(slot_budget.saturating_sub(1))
        };
        let primary_cap = slot_budget.saturating_sub(cont_take);
        if primary.len() > primary_cap {
            primary.truncate(primary_cap);
        }

        let mut merged =
            Vec::with_capacity(primary.len() + continuation_cands.len() + partial.len());
        let mut primary_iter = primary.into_iter();
        if let Some(top1) = primary_iter.next() {
            merged.push(top1);
        }
        let mut cont_iter = continuation_cands.into_iter();
        for _ in 0..cont_take {
            if let Some(c) = cont_iter.next() {
                merged.push(c);
            }
        }
        merged.extend(primary_iter);
        merged.extend(cont_iter);
        merged.extend(partial);

        self.candidates = merged;
        self.ensure_raw_and_limit(total_chars);

        Some(self.snapshot_state(false, true))
    }

    fn is_l3_pending(&self) -> bool {
        self.config.enable_neural
            && self.config.max_candidates > 0
            && !self.preedit.is_empty()
            && self.neural.backend_name() != "noop"
    }

    fn handle_char(&mut self, ch: char) -> EngineOutput {
        if self.preedit.is_empty() {
            if self.config.smart_spacing && self.last_auto_space && is_closing_punct(ch) {
                self.last_auto_space = false;
                let trimmed_len = self.context.before.trim_end_matches(' ').len();
                self.context.before.truncate(trimmed_len);
                let mut ch_buf = [0u8; 4];
                append_context_before(&mut self.context.before, ch.encode_utf8(&mut ch_buf), false);
                return EngineOutput::Commit(CommitAction {
                    seq: self.seq,
                    text: ch.to_string(),
                    delete_before: 1,
                    remaining_preedit: String::new(),
                    candidates: Vec::new(),
                });
            }
            if !is_word_char(ch) {
                self.last_auto_space = false;
                let mut ch_buf = [0u8; 4];
                append_context_before(&mut self.context.before, ch.encode_utf8(&mut ch_buf), false);
                return EngineOutput::State(self.current_state(true));
            }
        }

        if is_word_char(ch) {
            self.last_auto_space = false;
            self.preedit.push(ch);
            self.recompute_candidates();
            EngineOutput::State(self.current_state(false))
        } else if ch.is_ascii_graphic() {
            // 正在组合时遇到非词内可见标点/符号（如 ? . , ! ; : / ( ) 等）：
            // 自动将当前首选完整候选（去掉末尾空格）连同该符号按顺序一并上屏，
            // 防止符号在前端透传时抢先插到未落盘的 preedit 前面（如 "?rdp"）。
            if self.candidates.is_empty() {
                self.recompute_candidates();
            }
            let total_chars = self.preedit.chars().count();
            let chosen = self
                .candidates
                .iter()
                .find(|c| {
                    c.consumed_chars == total_chars
                        && c.kind != CandidateKind::Continuation
                        && c.kind != CandidateKind::Raw
                })
                .or_else(|| {
                    self.candidates.iter().find(|c| {
                        c.consumed_chars == total_chars && c.kind != CandidateKind::Continuation
                    })
                })
                .or_else(|| self.candidates.first())
                .map(|c| c.text.clone())
                .unwrap_or_else(|| self.preedit.clone());
            let base = chosen.trim_end_matches(' ');
            let learn_text = self.normalize_commit_for_learning(base);
            self.ngram
                .learn_commit(&self.context.before, &learn_text, self.privacy);

            let mut commit_text = String::with_capacity(base.len() + ch.len_utf8());
            commit_text.push_str(base);
            commit_text.push(ch);
            append_context_before(&mut self.context.before, &commit_text, false);

            self.preedit.clear();
            self.candidates.clear();
            self.token_lex_cache.clear();
            self.last_auto_space = false;

            EngineOutput::Commit(CommitAction {
                seq: self.seq,
                text: commit_text,
                delete_before: 0,
                remaining_preedit: String::new(),
                candidates: Vec::new(),
            })
        } else {
            // 正在组合时遇到其他非词控制字符先透传给宿主应用
            EngineOutput::State(self.current_state(true))
        }
    }

    fn handle_space(&mut self) -> EngineOutput {
        if self.preedit.is_empty() {
            self.last_auto_space = false;
            return EngineOutput::State(self.current_state(true));
        }

        if self.config.space_commits {
            if self.candidates.is_empty() {
                self.recompute_candidates();
            }
            if !self.candidates.is_empty() {
                return self.commit_candidate_at(0);
            }
            return EngineOutput::State(self.current_state(false));
        }

        // 默认保留词间空格在预编辑区中，支持 "i wnt to eat an appie" 这类多词组合
        if !self.preedit.ends_with(' ') {
            self.preedit.push(' ');
            self.recompute_candidates();
        }
        EngineOutput::State(self.current_state(false))
    }

    fn commit_candidate_at(&mut self, index: usize) -> EngineOutput {
        let cand = self.candidates[index].clone();
        let total_chars = self.preedit.chars().count();
        let consumed = cand.consumed_chars.min(total_chars);
        let remaining: String = self.preedit.chars().skip(consumed).collect();
        let remaining = remaining.trim_start_matches(' ').to_string();

        let learn_text = self.normalize_commit_for_learning(&cand.text);
        self.ngram
            .learn_commit(&self.context.before, &learn_text, self.privacy);

        let add_space =
            self.config.smart_spacing && cand.trailing_space && remaining.is_empty();
        let mut commit_text = cand.text.clone();
        if add_space {
            commit_text.push(' ');
        }

        // 无论完整上屏还是部分选词，都向前推进上下文窗口，使剩余 preedit 立即获得准确上文
        append_context_before(
            &mut self.context.before,
            &cand.text,
            add_space || !remaining.is_empty(),
        );

        self.last_auto_space = add_space;
        self.preedit = remaining.clone();
        self.token_lex_cache.clear();
        self.recompute_candidates();

        EngineOutput::Commit(CommitAction {
            seq: self.seq,
            text: commit_text,
            delete_before: 0,
            remaining_preedit: remaining,
            candidates: self.candidates.clone(),
        })
    }

    fn current_state(&self, passthrough: bool) -> PreeditState {
        PreeditState {
            seq: self.seq,
            revision: self.revision,
            cursor: self.preedit.chars().count(),
            preedit: self.preedit.clone(),
            candidates: self.candidates.clone(),
            passthrough,
            final_l3: passthrough || self.preedit.is_empty() || !self.is_l3_pending(),
        }
    }

    fn query_lexicon_cached(&mut self, token: &str, opts: &QueryOptions) -> Vec<LexiconCandidate> {
        if let Some(cached) = self.token_lex_cache.get(token) {
            return cached.clone();
        }
        let hits = self.lexicon.query(token, opts);
        if self.token_lex_cache.len() >= MAX_TOKEN_LEX_CACHE {
            self.token_lex_cache.clear();
        }
        self.token_lex_cache.insert(token.to_string(), hits.clone());
        hits
    }

    fn closed_neighbors_cached(&mut self, token: &str) -> Vec<LexiconCandidate> {
        if let Some(cached) = self.neighbor_cache.get(token) {
            return cached.clone();
        }
        let hits = self.lexicon.closed_token_neighbors(token, 12);
        if self.neighbor_cache.len() >= MAX_TOKEN_LEX_CACHE {
            self.neighbor_cache.clear();
        }
        self.neighbor_cache.insert(token.to_string(), hits.clone());
        hits
    }

    /// 若当前处于句首自动大写上下文（且用户原始输入首字符并非大写），
    /// 在写入 `user.db` 学习前将句首第一个词还原为词典原始大小写（如 "Now" -> "now"），
    /// 同时保留真正的专有名词与第一人称（如 "I"、"Monday"）。
    fn normalize_commit_for_learning(&self, text: &str) -> String {
        if !context_starts_sentence(&self.context.before) {
            return text.to_string();
        }
        if self
            .preedit
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        {
            return text.to_string();
        }
        let first_word = text
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '\'' && c != '-')
            .find(|s| !s.is_empty())
            .unwrap_or("");
        if first_word.is_empty()
            || !first_word
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
        {
            return text.to_string();
        }
        let first_lower = first_word.to_ascii_lowercase();
        let probe_opts = QueryOptions {
            limit: 1,
            enable_phonetic: false,
            ..QueryOptions::default()
        };
        let is_canonical_lower = self
            .lexicon
            .query(&first_lower, &probe_opts)
            .first()
            .is_some_and(|h| h.kind == LexiconMatchKind::Exact && h.word == first_lower);
        if !is_canonical_lower {
            return text.to_string();
        }
        let mut chars = text.chars();
        let Some(first_ch) = chars.next() else {
            return text.to_string();
        };
        let mut out = String::with_capacity(text.len());
        out.push(first_ch.to_ascii_lowercase());
        out.push_str(chars.as_str());
        out
    }

    fn recompute_candidates(&mut self) {
        self.candidates.clear();
        if self.preedit.is_empty() || self.config.max_candidates == 0 {
            return;
        }

        let total_chars = self.preedit.chars().count();
        let tokens = split_char_spans(&self.preedit);
        if tokens.is_empty() {
            self.push_raw_fallback(total_chars);
            return;
        }

        let opts = QueryOptions {
            limit: self.config.max_candidates,
            enable_phonetic: self.config.enable_phonetic,
            ..QueryOptions::default()
        };
        let sentence_start = context_starts_sentence(&self.context.before);
        let has_prev_word_ctx = !sentence_start && last_context_word(&self.context.before).is_some();

        if tokens.len() == 1 {
            let (tok, _, _) = &tokens[0];
            let is_closed_word = self.preedit.ends_with(' ');
            let lex_hits = self.query_lexicon_cached(tok, &opts);
            let expand_runon = should_expand_runon_token(tok, is_closed_word, &lex_hits);
            let runon_words = if expand_runon {
                self.lexicon.segment_runon(tok)
            } else {
                None
            };

            let mut kind_by_lower: Vec<(String, CandidateKind)> = Vec::with_capacity(lex_hits.len());
            let mut slot_cands: Vec<SlotCandidate> = Vec::with_capacity(lex_hits.len() + 4);
            for hit in lex_hits {
                kind_by_lower.push((hit.word.to_ascii_lowercase(), hit.kind.into()));
                slot_cands.push(SlotCandidate {
                    word: hit.word,
                    lex_score: hit.score,
                });
            }

            expand_slot_confusables(&self.lexicon, tok, &mut slot_cands, has_prev_word_ctx);
            slot_cands.sort_by(|a, b| {
                b.lex_score
                    .total_cmp(&a.lex_score)
                    .then_with(|| a.word.cmp(&b.word))
            });

            for sc in slot_cands {
                let low = sc.word.to_ascii_lowercase();
                let kind = kind_by_lower
                    .iter()
                    .find(|(k, _)| k == &low)
                    .map(|(_, kind)| *kind)
                    .unwrap_or(CandidateKind::TypoCorrection);
                let lm_bonus = self.ngram.score_in_context(&self.context.before, &sc.word);
                let text = if sentence_start {
                    capitalize_first_ascii(&sc.word)
                } else {
                    sc.word
                };
                if self
                    .candidates
                    .iter()
                    .any(|c| c.consumed_chars == total_chars && c.text == text)
                {
                    continue;
                }
                self.candidates.push(Candidate {
                    text,
                    kind,
                    comment: None,
                    score: sc.lex_score + lm_bonus,
                    consumed_chars: total_chars,
                    trailing_space: self.config.smart_spacing,
                });
            }

            if let Some(seg_words) = runon_words {
                let joined = format_sentence_proper_nouns(&seg_words.join(" "));
                let text = if sentence_start {
                    capitalize_first_ascii(&joined)
                } else {
                    joined
                };
                if !self
                    .candidates
                    .iter()
                    .any(|c| c.consumed_chars == total_chars && c.text == text)
                {
                    let best_single = self
                        .candidates
                        .iter()
                        .map(|c| c.score)
                        .fold(f32::NEG_INFINITY, f32::max);
                    let score = if expand_runon {
                        if best_single.is_finite() {
                            (best_single + 1.5).max(0.5)
                        } else {
                            0.5
                        }
                    } else if best_single.is_finite() {
                        best_single - 1.5
                    } else {
                        -2.0
                    };
                    self.candidates.push(Candidate {
                        text,
                        kind: CandidateKind::Sentence,
                        comment: Some("runon".into()),
                        score,
                        consumed_chars: total_chars,
                        trailing_space: self.config.smart_spacing,
                    });
                }
            }

            self.candidates
                .sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.text.cmp(&b.text)));
        } else {
            let mut slots = Vec::with_capacity(tokens.len() + 2);
            let mut first_hits: Vec<LexiconCandidate> = Vec::new();
            for (slot_idx, (tok, start_char, end_char)) in tokens.iter().enumerate() {
                let hits = self.query_lexicon_cached(tok, &opts);
                let is_closed_word = slot_idx + 1 < tokens.len() || self.preedit.ends_with(' ');
                if should_expand_runon_token(tok, is_closed_word, &hits) {
                    if let Some(seg_words) = self.lexicon.segment_runon(tok) {
                        if slot_idx == 0 {
                            first_hits.push(LexiconCandidate {
                                word: seg_words.join(" "),
                                kind: LexiconMatchKind::TypoCorrection,
                                score: 0.0,
                                log_freq: -2.0,
                                edit_cost: 0.5,
                            });
                        }
                        for sub_word in seg_words {
                            let sub_hits = self.query_lexicon_cached(&sub_word, &opts);
                            let mut sub_cands: Vec<SlotCandidate> = sub_hits
                                .into_iter()
                                .filter(|h| h.kind != LexiconMatchKind::Completion)
                                .map(|h| SlotCandidate {
                                    word: h.word,
                                    lex_score: h.score,
                                })
                                .collect();
                            if !sub_cands
                                .iter()
                                .any(|c| c.word.eq_ignore_ascii_case(&sub_word))
                            {
                                sub_cands.insert(
                                    0,
                                    SlotCandidate {
                                        word: sub_word.clone(),
                                        lex_score: 0.0,
                                    },
                                );
                            }
                            expand_slot_confusables(&self.lexicon, &sub_word, &mut sub_cands, true);
                            sub_cands.sort_by(|a, b| {
                                b.lex_score
                                    .total_cmp(&a.lex_score)
                                    .then_with(|| a.word.cmp(&b.word))
                            });
                            slots.push(LatticeSlot {
                                raw_token: sub_word,
                                start_char: *start_char,
                                end_char: *end_char,
                                candidates: sub_cands,
                            });
                        }
                        continue;
                    }
                }
                if slot_idx == 0 {
                    first_hits = hits.iter().take(2).cloned().collect();
                }
                let has_exact_hit = hits.iter().any(|h| h.kind == LexiconMatchKind::Exact);
                let tok_char_len = tok.chars().count();
                let best_phonetic: Option<SlotCandidate> = hits
                    .iter()
                    .find(|h| h.kind == LexiconMatchKind::PhoneticCorrection)
                    .map(|h| SlotCandidate {
                        word: h.word.clone(),
                        lex_score: h.score,
                    });
                let mut slot_cands: Vec<SlotCandidate> = hits
                    .into_iter()
                    .filter(|h| {
                        // 已按空格闭合的非末尾精确合法词不扩展前缀补全（如 build->building）；
                        // 闭合错拼词同样过滤超长前缀延伸（> tok_len + 2），为真实纠错词腾出 Viterbi 槽位。
                        if is_closed_word && h.kind == LexiconMatchKind::Completion {
                            if has_exact_hit {
                                return false;
                            }
                            if h.word.chars().count() > tok_char_len + 2 {
                                return false;
                            }
                        }
                        true
                    })
                    .map(|h| SlotCandidate {
                        word: h.word,
                        lex_score: h.score,
                    })
                    .collect();
                if slot_cands.is_empty() {
                    slot_cands.push(SlotCandidate {
                        word: tok.clone(),
                        lex_score: -50.0,
                    });
                }
                expand_slot_confusables(&self.lexicon, tok, &mut slot_cands, true);
                if is_closed_word {
                    let neighbors = self.closed_neighbors_cached(tok);
                    expand_closed_token_neighbors(&neighbors, tok, has_exact_hit, &mut slot_cands);
                }
                // 扩展易混词后按 lex_score 重排，防止高优 confusable 落到 MAX_CANDS_PER_SLOT 之外被截断
                slot_cands.sort_by(|a, b| {
                    b.lex_score
                        .total_cmp(&a.lex_score)
                        .then_with(|| a.word.cmp(&b.word))
                });
                // 为读音纠错（PhoneticCorrection，如 sikology->psychology）保底保留 1 个前 10 槽位
                if slot_cands.len() >= 10 {
                    if let Some(phon) = best_phonetic {
                        let in_top10 = slot_cands[..10]
                            .iter()
                            .any(|c| c.word.eq_ignore_ascii_case(&phon.word));
                        if !in_top10 {
                            slot_cands[9] = phon;
                        }
                    }
                }
                slots.push(LatticeSlot {
                    raw_token: tok.clone(),
                    start_char: *start_char,
                    end_char: *end_char,
                    candidates: slot_cands,
                });
            }

            let sentences =
                self.ngram
                    .decode_lattice(&self.context.before, &slots, self.config.max_candidates);
            for s in sentences {
                let formatted = format_sentence_proper_nouns(&s.text);
                let text = if sentence_start {
                    capitalize_first_ascii(&formatted)
                } else {
                    formatted
                };
                if self
                    .candidates
                    .iter()
                    .any(|c| c.consumed_chars == total_chars && c.text == text)
                {
                    continue;
                }
                self.candidates.push(Candidate {
                    text,
                    kind: CandidateKind::Sentence,
                    comment: Some("sentence".into()),
                    score: s.score,
                    consumed_chars: total_chars,
                    trailing_space: self.config.smart_spacing,
                });
            }

            // 同时保留首个词条的部分选词候选，便于用户只确认句首词
            let (_, _, first_end) = &tokens[0];
            for hit in first_hits {
                let text = if sentence_start {
                    capitalize_first_ascii(&hit.word)
                } else {
                    hit.word
                };
                if self
                    .candidates
                    .iter()
                    .any(|c| c.consumed_chars == *first_end && c.text == text)
                {
                    continue;
                }
                self.candidates.push(Candidate {
                    text,
                    kind: hit.kind.into(),
                    comment: Some("partial".into()),
                    score: hit.score - 1.0,
                    consumed_chars: *first_end,
                    trailing_space: false,
                });
            }
        }

        self.ensure_raw_and_limit(total_chars);
    }

    fn ensure_raw_and_limit(&mut self, total_chars: usize) {
        let max = self.config.max_candidates;
        let has_raw_exact = self
            .candidates
            .iter()
            .take(max)
            .any(|c| c.text == self.preedit && c.consumed_chars == total_chars);

        if has_raw_exact {
            self.candidates.truncate(max);
            return;
        }

        let keep = max.saturating_sub(1);
        self.candidates.truncate(keep);
        self.push_raw_fallback(total_chars);
    }

    fn push_raw_fallback(&mut self, total_chars: usize) {
        self.candidates.push(Candidate {
            text: self.preedit.clone(),
            kind: CandidateKind::Raw,
            comment: Some("raw".into()),
            score: -999.0,
            consumed_chars: total_chars,
            trailing_space: false,
        });
    }
}

fn append_context_before(before: &mut String, committed: &str, trailing_space: bool) {
    if !committed.is_empty() {
        before.push_str(committed);
    }
    if trailing_space && !before.ends_with(' ') {
        before.push(' ');
    }
    let count = before.chars().count();
    if count > MAX_CONTEXT_BEFORE_CHARS {
        let skip = count - MAX_CONTEXT_BEFORE_CHARS;
        if let Some((byte_idx, _)) = before.char_indices().nth(skip) {
            before.drain(..byte_idx);
        }
    }
}

fn context_starts_sentence(before: &str) -> bool {
    if before.is_empty() {
        return false;
    }
    if before.ends_with('\n') || before.ends_with('\r') {
        return true;
    }
    let trimmed = before.trim_end_matches([' ', '\t']);
    trimmed.ends_with('.') || trimmed.ends_with('!') || trimmed.ends_with('?')
}

fn capitalize_first_ascii(s: &str) -> String {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if !first.is_ascii_lowercase() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    out.push(first.to_ascii_uppercase());
    out.push_str(chars.as_str());
    out
}

/// 在多词整句模式下恢复英文星期、无歧义月份及介词后常见地名的首字母大写，
/// 避免影响单词模式（tokens.len() == 1）下的纯小写标识符输入。
fn format_sentence_proper_nouns(sentence: &str) -> String {
    const ALWAYS_PROPER_IN_SENTENCE: &[(&str, &str)] = &[
        ("monday", "Monday"),
        ("tuesday", "Tuesday"),
        ("wednesday", "Wednesday"),
        ("thursday", "Thursday"),
        ("friday", "Friday"),
        ("saturday", "Saturday"),
        ("sunday", "Sunday"),
        ("january", "January"),
        ("february", "February"),
        ("april", "April"),
        ("june", "June"),
        ("july", "July"),
        ("august", "August"),
        ("september", "September"),
        ("october", "October"),
        ("november", "November"),
        ("december", "December"),
    ];
    const PLACE_PROPER_AFTER_PREP: &[(&str, &str)] = &[
        ("cambridge", "Cambridge"),
        ("oxford", "Oxford"),
        ("london", "London"),
        ("europe", "Europe"),
        ("england", "England"),
    ];
    const PLACE_PREPOSITIONS: &[&str] = &["in", "at", "from", "to", "with", "near", "of", "for"];

    let mut out = String::with_capacity(sentence.len());
    let mut prev_lower = String::new();
    for (idx, tok) in sentence.split(' ').enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        let mut replacement: Option<&str> = None;
        if tok.bytes().all(|b| b.is_ascii_lowercase()) {
            for &(low, cap) in ALWAYS_PROPER_IN_SENTENCE {
                if tok == low {
                    replacement = Some(cap);
                    break;
                }
            }
            if replacement.is_none() && PLACE_PREPOSITIONS.iter().any(|&p| p == prev_lower) {
                for &(low, cap) in PLACE_PROPER_AFTER_PREP {
                    if tok == low {
                        replacement = Some(cap);
                        break;
                    }
                }
            }
        }
        let chosen = replacement.unwrap_or(tok);
        out.push_str(chosen);
        prev_lower = tok.to_ascii_lowercase();
    }
    out
}

/// 判断一个无空格的 token 是否应作为连打漏空格串优先展开为多词槽位：
/// - 只要词库存在 `Exact` 命中（如 `software`、`without`、`something`），绝不切分；
/// - 若存在显式驼峰或句首 `I` 连打边界（如 `Ilikethe`、`thatLikes`），优先允许切分；
/// - 否则若存在极低编辑代价的单键邻键纠错（`edit_cost <= 0.65`，如 `agrain->again`），
///   或未闭合词尾存在高频前缀补全（如 `internat->international`、`considera->consideration`），
///   优先保留单词纠错/补全，不在整句词格中强行拆碎。
fn should_expand_runon_token(tok: &str, is_closed_word: bool, hits: &[LexiconCandidate]) -> bool {
    if hits.iter().any(|h| h.kind == LexiconMatchKind::Exact) {
        return false;
    }
    let has_camel_or_i_prefix = tok.chars().skip(1).any(|c| c.is_ascii_uppercase())
        || (tok.starts_with('I') && tok.chars().nth(1).is_some_and(|c| c.is_ascii_lowercase()));
    if has_camel_or_i_prefix {
        return true;
    }
    let is_with_article = matches!(
        tok.to_ascii_lowercase().as_str(),
        "witha" | "ina" | "ona" | "fora" | "asa" | "isa" | "wasa" | "hasa" | "hada" | "nota"
    );
    if is_with_article {
        return true;
    }
    // 对纯小写无驼峰边界的词条：
    // - 只要存在单步邻键或单次增删转置以内的拼写纠错（edit_cost <= 1.25，如 "wht->why"、"teh->the"、"recieve->receive"），
    //   必定优先作为单词纠错，绝不拆碎；
    // - 若词长 >= 7 且最接近单字纠错的编辑代价已超过 1.25（如 "itwould" 对 "would" 的 edit_cost = 2.0，
    //   或 "thatlikes"、"takeyearys"、"dontknow"），允许尝试连打分词（由 segment_runon 的高频词均值门槛做二次把关）；
    // - 若词长 < 7（非 witha 类），只要词典能召回任何单字纠错/补全，均不拆分以守住短词延迟与准确率。
    if hits
        .iter()
        .any(|h| h.kind == LexiconMatchKind::TypoCorrection && h.edit_cost <= 1.25)
    {
        return false;
    }
    if tok.len() < 7 && !hits.is_empty() {
        return false;
    }
    if !is_closed_word
        && hits
            .iter()
            .any(|h| h.kind == LexiconMatchKind::Completion && h.log_freq >= -5.5)
    {
        return false;
    }
    true
}

fn split_char_spans(input: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut start = 0usize;
    let mut idx = 0usize;
    for ch in input.chars() {
        if ch == ' ' {
            if !cur.is_empty() {
                out.push((std::mem::take(&mut cur), start, idx));
            }
        } else {
            if cur.is_empty() {
                start = idx;
            }
            cur.push(ch);
        }
        idx += 1;
    }
    if !cur.is_empty() {
        out.push((cur, start, idx));
    }
    out
}

fn last_context_word(before: &str) -> Option<&str> {
    before
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '\'')
        .rfind(|s| !s.is_empty())
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '\'' || ch == '-'
}

fn is_closing_punct(ch: char) -> bool {
    matches!(ch, '.' | ',' | '?' | '!' | ';' | ':' | ')' | ']' | '"')
}

#[cfg(test)]
mod tests {
    use super::*;
    use anime_lexicon::{Lexicon, StaticWordList};
    use anime_lm_neural::NoopNeuralModel;
    use anime_lm_ngram::{NgramLanguageModel, NoopNgramModel};

    fn make_session() -> EngineSession<StaticWordList, NoopNgramModel, NoopNeuralModel> {
        EngineSession::new(
            StaticWordList::smoke_default(),
            NoopNgramModel,
            NoopNeuralModel,
        )
    }

    fn feed_text<L: LexiconProvider, N: NgramModel, M: NeuralSentenceModel>(
        sess: &mut EngineSession<L, N, M>,
        s: &str,
    ) -> PreeditState {
        let mut last = None;
        for ch in s.chars() {
            let key = if ch == ' ' {
                KeyInput::Space
            } else {
                KeyInput::Char(ch)
            };
            if let EngineOutput::State(st) = sess.handle_key(key) {
                last = Some(st);
            }
        }
        last.expect("at least one key fed")
    }

    #[test]
    fn space_stays_in_preedit_and_digit_one_commits_with_smart_space() {
        let mut sess = make_session();
        let st = feed_text(&mut sess, "i want an apple");
        assert!(!st.passthrough);
        assert_eq!(st.preedit, "i want an apple");
        assert_eq!(st.candidates[0].kind, CandidateKind::Sentence);
        assert_eq!(st.candidates[0].text, "I want an apple");

        match sess.handle_key(KeyInput::Digit(1)) {
            EngineOutput::Commit(c) => {
                assert_eq!(c.text, "I want an apple ");
                assert_eq!(c.remaining_preedit, "");
            }
            other => panic!("expected Commit, got {other:?}"),
        }

        // 紧接着按句点自动回删刚才附加的智能尾空格
        match sess.handle_key(KeyInput::Char('.')) {
            EngineOutput::Commit(c) => {
                assert_eq!(c.delete_before, 1);
                assert_eq!(c.text, ".");
            }
            other => panic!("expected punct delete-back Commit, got {other:?}"),
        }
    }

    #[test]
    fn enter_commits_raw_preedit_verbatim() {
        let mut sess = make_session();
        feed_text(&mut sess, "beaut");
        match sess.handle_key(KeyInput::Enter) {
            EngineOutput::Commit(c) => {
                assert_eq!(c.text, "beaut");
                assert_eq!(c.delete_before, 0);
            }
            other => panic!("expected raw Commit, got {other:?}"),
        }
        assert!(sess.preedit().is_empty());
    }

    #[test]
    fn backspace_and_escape_edit_and_clear_preedit() {
        let mut sess = make_session();
        feed_text(&mut sess, "app");
        match sess.handle_key(KeyInput::Backspace) {
            EngineOutput::State(st) => assert_eq!(st.preedit, "ap"),
            other => panic!("expected State, got {other:?}"),
        }
        match sess.handle_key(KeyInput::Escape) {
            EngineOutput::State(st) => {
                assert!(st.preedit.is_empty());
                assert!(!st.passthrough);
                assert!(st.candidates.is_empty());
            }
            other => panic!("expected cleared State, got {other:?}"),
        }
    }

    #[test]
    fn digits_and_space_passthrough_when_preedit_empty() {
        let mut sess = make_session();
        match sess.handle_key(KeyInput::Digit(1)) {
            EngineOutput::State(st) => assert!(st.passthrough),
            other => panic!("expected passthrough State, got {other:?}"),
        }
        match sess.handle_key(KeyInput::Space) {
            EngineOutput::State(st) => assert!(st.passthrough),
            other => panic!("expected passthrough State, got {other:?}"),
        }
    }

    #[test]
    fn partial_commit_keeps_remaining_preedit() {
        let mut sess = make_session();
        feed_text(&mut sess, "want apple");
        let partial_idx = sess
            .candidates()
            .iter()
            .position(|c| c.consumed_chars < "want apple".len())
            .expect("expected a partial candidate");
        let commit = sess.select(partial_idx).expect("select partial");
        assert_eq!(commit.text, "want");
        assert_eq!(commit.remaining_preedit, "apple");
        assert_eq!(sess.preedit(), "apple");
    }

    #[test]
    fn privacy_flag_toggles_cleanly() {
        let mut sess = make_session();
        assert!(!sess.is_privacy());
        sess.set_privacy(true);
        assert!(sess.is_privacy());
        sess.set_privacy(false);
        assert!(!sess.is_privacy());
    }

    #[test]
    fn context_realword_sentence_caps_shorthand_and_privacy_with_l2_lm() {
        let lex = Lexicon::from_word_zipfs(
            vec![
                ("go", 6.2),
                ("from", 6.8),
                ("form", 5.8),
                ("here", 6.1),
                ("fill", 5.4),
                ("out", 6.2),
                ("the", 7.7),
                ("over", 6.3),
                ("there", 6.5),
                ("their", 6.5),
                ("hope", 5.8),
                ("your", 6.7),
                ("you're", 6.1),
                ("happy", 5.6),
                ("think", 6.3),
                ("its", 6.6),
                ("it's", 6.55),
                ("raining", 4.6),
                ("how", 6.6),
                ("are", 7.0),
                ("you", 7.1),
                ("r", 3.2),
                ("u", 3.3),
                ("this", 6.9),
                ("is", 7.1),
                ("great", 6.0),
                ("secretword", 2.0),
            ],
            Vec::new(),
        );

        let lm = NgramLanguageModel::from_counts(
            vec![
                ("go", 200),
                ("from", 500),
                ("form", 300),
                ("here", 350),
                ("fill", 90),
                ("out", 220),
                ("the", 900),
                ("over", 240),
                ("there", 480),
                ("their", 460),
                ("i", 700),
                ("hope", 140),
                ("your", 450),
                ("you're", 220),
                ("happy", 130),
                ("think", 210),
                ("its", 380),
                ("this", 900),
                ("is", 1200),
                ("great", 250),
                ("it's", 340),
                ("raining", 80),
                ("how", 350),
                ("are", 600),
                ("you", 650),
                ("r", 5),
                ("u", 5),
            ],
            vec![
                ("go", "from", 90),
                ("from", "here", 120),
                ("out", "the", 85),
                ("the", "form", 70),
                ("over", "there", 130),
                ("hope", "you're", 80),
                ("you're", "happy", 70),
                ("think", "it's", 110),
                ("it's", "raining", 60),
                ("<bos>", "how", 160),
                ("how", "are", 150),
                ("are", "you", 145),
            ],
            vec![
                ("go", "from", "here", 70),
                ("out", "the", "form", 65),
                ("hope", "you're", "happy", 55),
                ("think", "it's", "raining", 50),
                ("<bos>", "how", "are", 120),
                ("how", "are", "you", 115),
            ],
        );

        let mut sess = EngineSession::new(lex, lm, NoopNeuralModel);

        // 1. 句首自动大写（空 context 不触发大写，仅句末标点后触发）
        sess.set_context("Hello. ", "");
        let st = feed_text(&mut sess, "this is great");
        assert_eq!(st.candidates[0].text, "This is great");

        // 2. 多词真词消歧 + 文本简写展开
        sess.reset_with_seq(10);
        sess.set_context("", "");
        assert_eq!(
            feed_text(&mut sess, "go form here").candidates[0].text,
            "go from here"
        );

        sess.reset_with_seq(20);
        assert_eq!(feed_text(&mut sess, "hw r u").candidates[0].text, "how are you");

        sess.reset_with_seq(30);
        sess.set_context("over ", "");
        assert_eq!(feed_text(&mut sess, "their").candidates[0].text, "there");

        sess.reset_with_seq(40);
        sess.set_context("fill out the ", "");
        assert_eq!(feed_text(&mut sess, "from").candidates[0].text, "form");

        sess.reset_with_seq(50);
        sess.set_context("I hope ", "");
        assert_eq!(
            feed_text(&mut sess, "your happy").candidates[0].text,
            "you're happy"
        );

        sess.reset_with_seq(60);
        sess.set_context("I think ", "");
        assert_eq!(
            feed_text(&mut sess, "its raining").candidates[0].text,
            "it's raining"
        );

        // 3. privacy=true 时 Enter 原样提交与数字选词均不记入个人学习库
        sess.reset_with_seq(70);
        sess.set_privacy(true);
        feed_text(&mut sess, "secretword");
        let _ = sess.handle_key_with_seq(71, KeyInput::Enter);
        assert_eq!(sess.ngram().user_store().word_freq("secretword"), 0);

        // 4. space_commits=true 时空格直接提交第 1 候选
        sess.set_privacy(false);
        sess.set_config(EngineConfig {
            space_commits: true,
            ..EngineConfig::default()
        });
        feed_text(&mut sess, "form");
        match sess.handle_key_with_seq(80, KeyInput::Space) {
            EngineOutput::Commit(cm) => {
                assert_eq!(cm.seq, 80);
                assert_eq!(cm.text, "form ");
                assert_eq!(sess.ngram().user_store().word_freq("form"), 1);
            }
            other => panic!("expected space_commits Commit, got {other:?}"),
        }
    }

    #[test]
    fn neural_rerank_respects_seq_invariants_and_bumps_revision() {
        struct MockNeural;
        impl NeuralSentenceModel for MockNeural {
            fn backend_name(&self) -> &'static str {
                "mock-l3"
            }
            fn rerank_sentences(&self, _: &str, _: &str, _: &[String]) -> Vec<NeuralCandidate> {
                Vec::new()
            }
            fn predict_continuations(&self, _: &str, _: &str, _: usize) -> Vec<NeuralCandidate> {
                Vec::new()
            }
        }

        let mut sess = EngineSession::new(
            StaticWordList::smoke_default(),
            NoopNgramModel,
            MockNeural,
        );
        let st = match sess.handle_key_with_seq(100, KeyInput::Char('t')) {
            EngineOutput::State(s) => s,
            _ => panic!("expected State"),
        };
        assert_eq!(st.seq, 100);
        assert_eq!(st.revision, 0);
        assert!(!st.final_l3);

        // 过期 seq 必须严格丢弃
        assert!(
            sess.apply_neural_rerank(
                99,
                &[],
                &[NeuralCandidate {
                    text: "tomorrow".into(),
                    score: 10.0,
                    is_continuation: true,
                }]
            )
            .is_none()
        );
        assert_eq!(sess.revision(), 0);

        // 匹配 seq=100 时升版至 revision=1, final_l3=true 并保留 Raw 兜底
        let pushed = sess
            .apply_neural_rerank(
                100,
                &[NeuralCandidate {
                    text: "to".into(),
                    score: 15.0,
                    is_continuation: false,
                }],
                &[NeuralCandidate {
                    text: "the moon".into(),
                    score: 4.0,
                    is_continuation: true,
                }],
            )
            .expect("apply matching seq");
        assert_eq!(pushed.seq, 100);
        assert_eq!(pushed.revision, 1);
        assert!(pushed.final_l3);
        assert_eq!(pushed.candidates[0].text, "to");
        assert!(pushed
            .candidates
            .iter()
            .any(|c| c.kind == CandidateKind::Continuation && c.text == "the moon"));
        assert!(pushed
            .candidates
            .iter()
            .any(|c| c.kind == CandidateKind::Raw && c.text == "t"));
    }

    #[test]
    fn neural_rerank_reorders_candidates_and_appends_unique_continuations() {
        struct MockNeural;
        impl NeuralSentenceModel for MockNeural {
            fn backend_name(&self) -> &'static str {
                "mock-l3"
            }
            fn rerank_sentences(&self, _: &str, _: &str, _: &[String]) -> Vec<NeuralCandidate> {
                Vec::new()
            }
            fn predict_continuations(&self, _: &str, _: &str, _: usize) -> Vec<NeuralCandidate> {
                Vec::new()
            }
        }

        let lex = Lexicon::from_word_zipfs(vec![("from", 7.0), ("form", 5.5)], Vec::new());
        let mut sess = EngineSession::new(lex, NoopNgramModel, MockNeural);
        sess.set_context("Thank you for your ", "");

        let mut last = None;
        for (idx, ch) in ['f', 'r', 'o', 'm'].into_iter().enumerate() {
            if let EngineOutput::State(st) =
                sess.handle_key_with_seq((idx + 1) as u64, KeyInput::Char(ch))
            {
                last = Some(st);
            }
        }
        let st = last.expect("state after 'from'");
        assert_eq!(st.revision, 0);
        assert!(!st.final_l3);
        assert_eq!(st.candidates[0].text, "from");

        // Rerank boosts "form" over "from", appends unique "✦" continuation, and keeps raw text.
        let pushed = sess
            .apply_neural_rerank(
                st.seq,
                &[
                    NeuralCandidate {
                        text: "form".into(),
                        score: 12.0,
                        is_continuation: false,
                    },
                    NeuralCandidate {
                        text: "from".into(),
                        score: -2.0,
                        is_continuation: false,
                    },
                ],
                &[
                    NeuralCandidate {
                        text: "form".into(),
                        score: -0.5,
                        is_continuation: true,
                    },
                    NeuralCandidate {
                        text: "form submission".into(),
                        score: -1.0,
                        is_continuation: true,
                    },
                    NeuralCandidate {
                        text: "FORM SUBMISSION".into(),
                        score: -1.2,
                        is_continuation: true,
                    },
                ],
            )
            .expect("apply neural rerank");

        assert_eq!(pushed.revision, 1);
        assert!(pushed.final_l3);
        assert_eq!(pushed.candidates[0].text, "form");
        assert!(pushed.candidates.iter().any(|c| c.text == "from"));
        let conts: Vec<&Candidate> = pushed
            .candidates
            .iter()
            .filter(|c| c.kind == CandidateKind::Continuation)
            .collect();
        assert_eq!(conts.len(), 1);
        assert_eq!(conts[0].text, "form submission");
        assert_eq!(conts[0].comment.as_deref(), Some("✦"));
    }

    #[test]
    fn neural_rerank_reserves_slots_for_continuations_when_primary_full() {
        struct MockNeural;
        impl NeuralSentenceModel for MockNeural {
            fn backend_name(&self) -> &'static str {
                "mock-l3"
            }
            fn rerank_sentences(&self, _: &str, _: &str, _: &[String]) -> Vec<NeuralCandidate> {
                Vec::new()
            }
            fn predict_continuations(&self, _: &str, _: &str, _: usize) -> Vec<NeuralCandidate> {
                Vec::new()
            }
        }

        let lex = Lexicon::from_word_zipfs(
            vec![
                ("for", 7.0),
                ("form", 6.2),
                ("from", 6.1),
                ("force", 5.8),
                ("forest", 5.6),
                ("forget", 5.5),
                ("former", 5.4),
                ("forward", 5.3),
                ("fortune", 5.2),
                ("foreign", 5.1),
            ],
            Vec::new(),
        );
        let mut sess = EngineSession::new(lex, NoopNgramModel, MockNeural);
        sess.set_context("Thank you ", "");

        let mut last = None;
        for (idx, ch) in ['f', 'o', 'r', ' '].into_iter().enumerate() {
            let key = if ch == ' ' {
                KeyInput::Space
            } else {
                KeyInput::Char(ch)
            };
            if let EngineOutput::State(st) = sess.handle_key_with_seq((idx + 1) as u64, key) {
                last = Some(st);
            }
        }
        let st = last.expect("state for 'for '");
        assert_eq!(st.candidates.len(), sess.config().max_candidates);

        let pushed = sess
            .apply_neural_rerank(
                st.seq,
                &[],
                &[
                    NeuralCandidate {
                        text: "for your".into(),
                        score: -0.6,
                        is_continuation: true,
                    },
                    NeuralCandidate {
                        text: "for the".into(),
                        score: -0.9,
                        is_continuation: true,
                    },
                    NeuralCandidate {
                        text: "for this".into(),
                        score: -1.4,
                        is_continuation: true,
                    },
                ],
            )
            .expect("apply continuations");

        assert!(pushed.final_l3);
        assert_eq!(pushed.candidates.len(), sess.config().max_candidates);
        assert_eq!(pushed.candidates[0].text, "for");
        assert_eq!(pushed.candidates[1].text, "for your");
        assert_eq!(pushed.candidates[1].kind, CandidateKind::Continuation);
        assert_eq!(pushed.candidates[2].text, "for the");
        assert_eq!(pushed.candidates[2].kind, CandidateKind::Continuation);
        let conts: Vec<&str> = pushed
            .candidates
            .iter()
            .filter(|c| c.kind == CandidateKind::Continuation && c.comment.as_deref() == Some("✦"))
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(conts, vec!["for your", "for the"]);
        assert_eq!(
            pushed.candidates.last().map(|c| c.kind),
            Some(CandidateKind::Raw)
        );
    }

    #[test]
    fn closed_exact_word_skips_completions_and_confusables_survive_slot_cap() {
        // 造出 >= 12 个以 "we" / "the" / "build" 开头的词，验证闭合精确词不会被扩展成前缀补全，
        // 且 where/were/we're/wear 易混词不会被 MAX_CANDS_PER_SLOT=10 截断。
        let lex = Lexicon::from_word_zipfs(
            vec![
                ("the", 7.7),
                ("build", 5.8),
                ("building", 6.2),
                ("buildings", 5.9),
                ("builder", 5.4),
                ("passed", 5.6),
                ("where", 6.8),
                ("were", 6.7),
                ("happy", 5.9),
                ("whereby", 4.5),
                ("wherein", 4.4),
                ("whereof", 4.3),
                ("whereas", 5.0),
                ("wherever", 5.1),
                ("wherewith", 3.5),
                ("wheresoever", 3.2),
                ("whereabouts", 4.2),
                ("whereupon", 4.1),
                ("wherefore", 4.0),
                ("there", 6.6),
                ("their", 6.5),
                ("they're", 6.2),
                ("coming", 5.7),
                ("thereby", 5.0),
                ("therefore", 5.5),
                ("therein", 4.6),
                ("thereof", 4.7),
                ("thereon", 4.1),
                ("thereto", 3.8),
                ("therewith", 3.9),
                ("theirs", 4.8),
                ("themselves", 5.6),
            ],
            Vec::new(),
        );
        let lm = NgramLanguageModel::from_counts(
            vec![
                ("the", 900),
                ("build", 200),
                ("building", 600),
                ("passed", 250),
                ("there", 500),
                ("their", 450),
                ("they're", 420),
                ("coming", 260),
                ("where", 480),
                ("were", 460),
                ("happy", 240),
            ],
            vec![
                ("the", "building", 180),
                ("building", "passed", 140),
                ("the", "build", 90),
                ("build", "passed", 90),
                ("they're", "coming", 160),
                ("were", "happy", 150),
            ],
            Vec::<(&str, &str, &str, u32)>::new(),
        );
        let mut sess = EngineSession::new(lex, lm, NoopNeuralModel);
        let st = feed_text(&mut sess, "the build passed");
        assert!(
            !st.candidates.iter().any(|c| c.text.contains("building")),
            "closed exact word 'build' should not expand to 'building': {:?}",
            st.candidates
        );

        sess.reset();
        let st2 = feed_text(&mut sess, "there coming");
        assert!(
            st2.candidates.iter().any(|c| c.text == "they're coming"),
            "expected confusable 'they're coming' inside candidates despite 10+ 'there*' completions: {:?}",
            st2.candidates
        );

        sess.reset();
        let st3 = feed_text(&mut sess, "where happy");
        assert!(
            st3.candidates.iter().any(|c| c.text == "were happy"),
            "expected 'where' confusable 'were' inside candidates: {:?}",
            st3.candidates
        );
    }

    #[test]
    fn punctuation_commits_composing_preedit_in_order_and_digit_clears_auto_space() {
        let mut sess = make_session();
        // 1. 组合态下直接敲 '?' 应把当前首选候选 + '?' 合并顺序上屏（绝不能透传导致 "?apple"）
        let _ = feed_text(&mut sess, "apple");
        match sess.handle_key(KeyInput::Char('?')) {
            EngineOutput::Commit(c) => {
                assert_eq!(c.text, "apple?");
                assert_eq!(c.delete_before, 0);
                assert!(c.remaining_preedit.is_empty());
            }
            other => panic!("expected Commit('apple?'), got {other:?}"),
        }
        assert!(sess.preedit().is_empty());

        // 2. 上一句以 '?' 结尾后，新句首词自动大写为 "Apple "；
        //    随后若输入数字 '6' 再输入 '.'，绝不能触发 delete_before=1 误删 '6'
        let _ = feed_text(&mut sess, "apple");
        match sess.handle_key(KeyInput::Digit(1)) {
            EngineOutput::Commit(c) => assert_eq!(c.text, "Apple "),
            other => panic!("expected Commit('Apple '), got {other:?}"),
        }
        match sess.handle_key(KeyInput::Digit(6)) {
            EngineOutput::State(st) => assert!(st.passthrough),
            other => panic!("expected passthrough State for digit 6, got {other:?}"),
        }
        match sess.handle_key(KeyInput::Char('.')) {
            EngineOutput::State(st) => assert!(st.passthrough),
            other => panic!("expected passthrough State for '.' after digit 6, got {other:?}"),
        }
    }

    #[test]
    fn closed_token_neighbors_recover_leading_drop_but_keep_clean_words() {
        let lex = Lexicon::from_word_zipfs(
            vec![
                ("i", 7.0),
                ("want", 6.0),
                ("to", 7.4),
                ("of", 7.5),
                ("or", 6.6),
                ("go", 6.2),
                ("home", 5.8),
                ("at", 6.9),
                ("the", 7.7),
                ("he", 6.7),
                ("end", 5.6),
                ("said", 6.1),
                ("hen", 2.5),
            ],
            Vec::new(),
        );
        let lm = NgramLanguageModel::from_counts(
            vec![("i", 900), ("want", 400), ("to", 1500), ("go", 300), ("home", 200), ("at", 600),
                 ("the", 3000), ("he", 900), ("end", 150), ("said", 300), ("of", 1700), ("or", 400)],
            vec![("i", "want", 300), ("want", "to", 280), ("to", "go", 200), ("go", "home", 120),
                 ("at", "the", 400), ("the", "end", 120), ("he", "said", 200)],
            vec![("i", "want", "to", 200), ("want", "to", "go", 150), ("at", "the", "end", 100)],
        );
        let mut sess = EngineSession::new(lex, lm, NoopNeuralModel);

        // 单字母非词 o 已被空格闭合：一次编辑近邻召回 to，由 L2 选中
        let st = feed_text(&mut sess, "i want o go home");
        assert_eq!(st.candidates[0].text.to_ascii_lowercase(), "i want to go home");

        // 真词 he 在明确语境下翻为 the
        sess.reset();
        let st = feed_text(&mut sess, "at he end");
        assert_eq!(st.candidates[0].text.to_ascii_lowercase(), "at the end");

        // 无反向证据时保留用户键入的真词
        sess.reset();
        let st = feed_text(&mut sess, "he said");
        assert_eq!(st.candidates[0].text.to_ascii_lowercase(), "he said");

        // 罕见但合法的原词（Zipf < 3）不展开真词近邻，即便 L2 偏好常见近邻 he
        sess.reset();
        let st = feed_text(&mut sess, "hen said");
        assert_eq!(st.candidates[0].text.to_ascii_lowercase(), "hen said");

        // 正在输入的末尾 token 不走闭合近邻（单词模式前缀行为不变）
        sess.reset();
        let st = feed_text(&mut sess, "o");
        assert!(st.candidates.iter().all(|c| c.text.to_ascii_lowercase() != "to"));
    }

    #[test]
    fn smart_runon_word_segmentation_in_single_and_sentence_mode() {
        let lex = Lexicon::from_word_zipfs(
            vec![
                ("I", 7.1),
                ("like", 6.5),
                ("likes", 5.6),
                ("the", 7.6),
                ("software", 5.8),
                ("that", 7.0),
                ("me", 6.7),
                ("with", 6.9),
                ("a", 7.4),
                ("don't", 6.6),
                ("know", 6.5),
                ("why", 6.8),
                ("it", 7.3),
                ("would", 6.8),
                ("take", 6.6),
                ("years", 6.7),
                ("of", 7.5),
                ("working", 6.2),
                ("to", 7.5),
                ("have", 7.1),
                ("new", 6.8),
            ],
            Vec::new(),
        );
        let lm = NgramLanguageModel::from_counts(
            vec![
                ("I", 800),
                ("like", 400),
                ("likes", 220),
                ("the", 950),
                ("software", 300),
                ("that", 600),
                ("me", 450),
                ("with", 500),
                ("a", 850),
                ("don't", 420),
                ("know", 410),
                ("why", 450),
                ("it", 800),
                ("would", 500),
                ("take", 420),
                ("years", 430),
                ("of", 900),
                ("working", 320),
                ("to", 900),
                ("have", 700),
                ("new", 500),
            ],
            vec![
                ("I", "like", 260),
                ("like", "the", 240),
                ("the", "software", 190),
                ("software", "that", 160),
                ("that", "likes", 180),
                ("likes", "me", 190),
                ("with", "a", 210),
                ("don't", "know", 230),
                ("why", "it", 220),
                ("it", "would", 260),
                ("would", "take", 240),
                ("take", "years", 230),
                ("years", "of", 240),
                ("of", "the", 220),
                ("the", "working", 180),
                ("working", "to", 170),
                ("to", "have", 250),
                ("have", "new", 190),
            ],
            Vec::<(&str, &str, &str, u32)>::new(),
        );
        let mut sess = EngineSession::new(lex, lm, NoopNeuralModel);

        // 1. 单 token 连打分词："Ilikethe" -> "I like the"
        let st_single = feed_text(&mut sess, "Ilikethe");
        assert_eq!(st_single.candidates[0].text, "I like the");

        // 2. 缩略词省撇号 + 连打："dontknow" -> "don't know"
        sess.reset();
        let st_contr = feed_text(&mut sess, "dontknow");
        assert_eq!(st_contr.candidates[0].text, "don't know");

        // 3. 整句混合连打与合法复合词（software 不被拆开，Ilikethe 与 thatlikes 自动切分）
        sess.reset();
        let st_sent = feed_text(&mut sess, "Ilikethe software thatlikes me");
        assert_eq!(
            st_sent.candidates[0].text,
            "I like the software that likes me",
            "expected segmented sentence at top-1, got: {:?}",
            st_sent.candidates
        );

        // 4. 整句同时包含单词邻键改错（wht->why）、精确连打（itwould->it would）与带错连打（takeyearys->take years）
        sess.reset();
        let st_mixed = feed_text(&mut sess, "wht itwould takeyearys of the working to have new");
        assert_eq!(
            st_mixed.candidates[0].text,
            "why it would take years of the working to have new",
            "expected joint typo-correction + runon segmentation at top-1, got: {:?}",
            st_mixed.candidates
        );
    }
}
