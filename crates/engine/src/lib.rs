//! 平台无关英文输入法会话状态机：管理预编辑区、多路候选融合、句首大写、易混词与上下文重排、
//! 数字/部分选词、智能空格/标点回删、`seq`/`revision` 抗过期控制与 L3 异步结果合并。

mod confusables;
mod profile;

use std::collections::HashMap;

use anime_lexicon::{LexiconCandidate, LexiconMatchKind, LexiconProvider, QueryOptions};
use anime_lm_neural::{NeuralCandidate, NeuralSentenceModel};
use anime_lm_ngram::{LatticeSlot, NgramModel, SlotCandidate};
use anime_protocol::{
    Candidate, CandidateKind, CommitAction, ContextText, EngineConfig, KeyInput, PreeditState,
};

use crate::confusables::{expand_closed_token_neighbors, expand_slot_confusables};
pub use crate::profile::{
    BASELINE_PROFILE, ConfusableProfile, ContextLmProfile, L3Profile, LatticeProfile,
    LexiconProfile, ModeProfile, ModeProfiles, PartialProfile, RunonProfile, SENTENCE_PROFILE,
    SpaceEditProfile, VerbatimProfile, WORD_PROFILE,
};

const MAX_CONTEXT_BEFORE_CHARS: usize = 256;
const MAX_TOKEN_LEX_CACHE: usize = 256;
// 打分 / 召回参数按模式存放在 `profile::ModeProfile`（单词模式 / 整句模式各一份），见该模块文档。

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
    /// 单词 / 整句两份打分参数；每次重算按 `config.space_commits` 取当前模式那份。
    profiles: ModeProfiles,
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
    slot_cache: HashMap<(String, bool), (Vec<LatticeSlot>, Vec<LexiconCandidate>)>,
    exact_probe_cache: HashMap<String, Option<f32>>,
    /// 按 token 缓存"大小写意图"判定：驼峰词需额外探测近邻规范词形，每击对整句所有 token 重算会吃掉同步预算。
    verbatim_lead_cache: HashMap<String, bool>,
    /// 最近一次上屏实际写入个人学习库的（句首归一化后）文本，供宿主用同一文本同步 L1 动态层。
    last_learned_text: Option<String>,
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
            profiles: ModeProfiles::default(),
            context: ContextText::default(),
            privacy: false,
            preedit: String::new(),
            candidates: Vec::new(),
            seq: 0,
            revision: 0,
            last_auto_space: false,
            token_lex_cache: HashMap::with_capacity(16),
            neighbor_cache: HashMap::with_capacity(16),
            slot_cache: HashMap::with_capacity(32),
            exact_probe_cache: HashMap::with_capacity(64),
            verbatim_lead_cache: HashMap::with_capacity(16),
            last_learned_text: None,
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// 换配置后立即按新的 `space_commits` 选用对应模式的 profile 并重算候选。
    pub fn set_config(&mut self, config: EngineConfig) {
        self.config = config;
        self.revision = 0;
        self.clear_scoring_caches();
        self.recompute_candidates();
    }

    /// 当前模式（由 `config.space_commits` 决定）生效的打分参数。
    pub fn profile(&self) -> &ModeProfile {
        self.profiles.for_space_commits(self.config.space_commits)
    }

    pub fn profiles(&self) -> &ModeProfiles {
        &self.profiles
    }

    /// 覆盖两种模式的打分参数（调参 / 评测用），立即重算候选。
    pub fn set_profiles(&mut self, profiles: ModeProfiles) {
        self.profiles = profiles;
        self.revision = 0;
        self.clear_scoring_caches();
        self.recompute_candidates();
    }

    /// 槽位、近邻与大小写意图缓存都依赖查询上限 / profile 参数，配置或 profile 变化时必须整体失效。
    fn clear_scoring_caches(&mut self) {
        self.token_lex_cache.clear();
        self.neighbor_cache.clear();
        self.slot_cache.clear();
        self.verbatim_lead_cache.clear();
    }

    fn query_options(&self) -> QueryOptions {
        let p = self.profile();
        QueryOptions {
            limit: p.lexicon.query_limit.unwrap_or(self.config.max_candidates),
            enable_phonetic: self.config.enable_phonetic,
            max_edit_cost: p.lexicon.max_edit_cost,
            exhaustive_single_edit: p.lexicon.exhaustive_single_edit,
        }
    }

    /// 精确词基准分中的固定加成（L1 `EXACT_BONUS` + profile 精确词偏移），供易混词锚点还原原词词频。
    fn exact_anchor_bonus(&self) -> f32 {
        let p = self.profile();
        p.confusables.lexicon_exact_bonus + p.lexicon.exact_offset
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
        self.verbatim_lead_cache.clear();
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
                    let learn_text = self.normalize_commit_for_learning(&self.preedit.clone());
                    let raw = std::mem::take(&mut self.preedit);
                    self.candidates.clear();
                    self.token_lex_cache.clear();
                    self.verbatim_lead_cache.clear();
                    self.last_auto_space = false;
                    self.learn(&learn_text);
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
                    self.verbatim_lead_cache.clear();
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

    /// 当前预编辑是否应请求异步 L3 重排（与后端是否加载无关）；离线评测据此与 animed 调度口径保持一致。
    pub fn wants_l3_rerank(&self) -> bool {
        let l3 = self.profile().l3;
        self.config.enable_neural
            && self.config.max_candidates > 0
            && !self.preedit.is_empty()
            && l3.enabled
            && (l3.min_tokens <= 1 || self.preedit.split_whitespace().count() >= l3.min_tokens)
    }

    fn is_l3_pending(&self) -> bool {
        self.wants_l3_rerank() && self.neural.backend_name() != "noop"
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
            self.learn(&learn_text);

            let mut commit_text = String::with_capacity(base.len() + ch.len_utf8());
            commit_text.push_str(base);
            commit_text.push(ch);
            append_context_before(&mut self.context.before, &commit_text, false);

            self.preedit.clear();
            self.candidates.clear();
            self.token_lex_cache.clear();
            self.verbatim_lead_cache.clear();
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
        self.learn(&learn_text);

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
        self.verbatim_lead_cache.clear();
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
        let hits = if let Some(cached) = self.token_lex_cache.get(token) {
            cached.clone()
        } else {
            let hits = self.lexicon.query(token, opts);
            if self.token_lex_cache.len() >= MAX_TOKEN_LEX_CACHE {
                self.token_lex_cache.clear();
            }
            self.token_lex_cache.insert(token.to_string(), hits.clone());
            hits
        };
        self.apply_kind_offsets(hits)
    }

    /// 按匹配类型施加 profile 偏移并按分数重排；全零偏移时原样返回（保持 L1 原始次序）。
    fn apply_kind_offsets(&self, mut hits: Vec<LexiconCandidate>) -> Vec<LexiconCandidate> {
        let lp = self.profile().lexicon;
        if !lp.has_offsets() {
            return hits;
        }
        for h in &mut hits {
            h.score += match h.kind {
                LexiconMatchKind::Exact => lp.exact_offset,
                LexiconMatchKind::Completion => lp.completion_offset,
                LexiconMatchKind::TypoCorrection => lp.typo_offset,
                LexiconMatchKind::PhoneticCorrection => lp.phonetic_offset,
            };
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits
    }

    /// 按 token 切分构建整句词格槽位；同时返回首词的部分选词候选。
    fn build_sentence_slots(
        &mut self,
        tokens: &[(String, usize, usize)],
        opts: &QueryOptions,
    ) -> (Vec<LatticeSlot>, Vec<LexiconCandidate>) {
        let ends_with_space = self.preedit.ends_with(' ');
        let mut slots = Vec::with_capacity(tokens.len() + 2);
        let mut first_hits: Vec<LexiconCandidate> = Vec::new();
        for (slot_idx, (tok, start_char, end_char)) in tokens.iter().enumerate() {
            let is_closed_word = slot_idx + 1 < tokens.len() || ends_with_space;
            let key = (tok.clone(), is_closed_word);
            let (tok_slots, tok_first) = match self.slot_cache.get(&key) {
                Some(cached) => cached.clone(),
                None => {
                    let built = self.build_token_slots(tok, is_closed_word, opts);
                    if self.slot_cache.len() >= MAX_TOKEN_LEX_CACHE * 2 {
                        self.slot_cache.clear();
                    }
                    self.slot_cache.insert(key, built.clone());
                    built
                }
            };
            if slot_idx == 0 {
                first_hits = tok_first;
            }
            // 缓存的槽位与 token 字符区间无关，按当前切分回填区间
            slots.extend(tok_slots.into_iter().map(|mut sl| {
                sl.start_char = *start_char;
                sl.end_char = *end_char;
                sl
            }));
        }
        (slots, first_hits)
    }

    /// 单个 token 的词格槽位（连打拆分时可能是多个槽位）与作为首词时的部分选词候选。
    /// 只依赖 token 文本与是否已闭合，可跨按键、跨备选切分缓存。
    fn build_token_slots(
        &mut self,
        tok: &str,
        is_closed_word: bool,
        opts: &QueryOptions,
    ) -> (Vec<LatticeSlot>, Vec<LexiconCandidate>) {
        let (start_char, end_char) = (0usize, tok.chars().count());
        let tok = &tok.to_string();
        let p = *self.profile();
        let anchor_bonus = self.exact_anchor_bonus();
        let mut slots: Vec<LatticeSlot> = Vec::with_capacity(2);
        let mut first_hits: Vec<LexiconCandidate> = Vec::new();
        let hits = self.query_lexicon_cached(tok, opts);
        if should_expand_runon_token(tok, is_closed_word, &hits, &p.runon) {
            if let Some(seg_words) = self.lexicon.segment_runon(tok) {
                first_hits.push(LexiconCandidate {
                    word: seg_words.join(" "),
                    kind: LexiconMatchKind::TypoCorrection,
                    score: 0.0,
                    log_freq: -2.0,
                    edit_cost: 0.5,
                });
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
                    expand_slot_confusables(
                        &self.lexicon,
                        &sub_word,
                        &mut sub_cands,
                        true,
                        false,
                        &p.confusables,
                        anchor_bonus,
                    );
                    sub_cands.sort_by(|a, b| {
                        b.lex_score
                            .total_cmp(&a.lex_score)
                            .then_with(|| a.word.cmp(&b.word))
                    });
                    if let Some(cap) = p.lattice.max_slot_candidates {
                        sub_cands.truncate(cap.max(1));
                    }
                    slots.push(LatticeSlot {
                        raw_token: sub_word,
                        start_char,
                        end_char,
                        candidates: sub_cands,
                    });
                }
                return (slots, first_hits);
            }
        }
        first_hits = hits.iter().take(p.partial.first_word_hits).cloned().collect();
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
                    if h.word.chars().count() > tok_char_len + p.lattice.closed_completion_max_extra_chars {
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
        expand_slot_confusables(
            &self.lexicon,
            tok,
            &mut slot_cands,
            true,
            false,
            &p.confusables,
            anchor_bonus,
        );
        if is_closed_word {
            let neighbors = self.closed_neighbors_cached(tok);
            expand_closed_token_neighbors(
                &neighbors,
                tok,
                has_exact_hit,
                &mut slot_cands,
                &p.confusables,
                anchor_bonus,
            );
        }
        // 扩展易混词后按 lex_score 重排，防止高优 confusable 落到 MAX_CANDS_PER_SLOT 之外被截断
        slot_cands.sort_by(|a, b| {
            b.lex_score
                .total_cmp(&a.lex_score)
                .then_with(|| a.word.cmp(&b.word))
        });
        // 为读音纠错（PhoneticCorrection，如 sikology->psychology）保底保留 1 个前 N 槽位（默认 10，
        // 与 L2 的 MAX_CANDS_PER_SLOT 对齐，否则保底位会被词格截掉）
        let window = p.lattice.phonetic_reserve_window;
        if window > 0 && slot_cands.len() >= window {
            if let Some(phon) = best_phonetic {
                let in_window = slot_cands[..window]
                    .iter()
                    .any(|c| c.word.eq_ignore_ascii_case(&phon.word));
                if !in_window {
                    slot_cands[window - 1] = phon;
                }
            }
        }
        if let Some(cap) = p.lattice.max_slot_candidates {
            slot_cands.truncate(cap.max(1));
        }
        slots.push(LatticeSlot {
            raw_token: tok.clone(),
            start_char,
            end_char,
            candidates: slot_cands,
        });
        (slots, first_hits)
    }

    fn is_exact_word(&mut self, word: &str, opts: &QueryOptions) -> bool {
        self.exact_log_freq(word, opts).is_some()
    }

    /// 只判定"是否为词典词及其词频"：编辑预算 0、关闭读音，跳过模糊束搜索与语音桶扫描。
    /// 备选切分要探测大量碎片（合并串、错位串、两段切分），走完整查询会把同步路径拖到数毫秒。
    fn exact_log_freq(&mut self, word: &str, _opts: &QueryOptions) -> Option<f32> {
        if let Some(&cached) = self.exact_probe_cache.get(word) {
            return cached;
        }
        let probe = QueryOptions {
            limit: 3,
            enable_phonetic: false,
            max_edit_cost: 0.0,
            exhaustive_single_edit: false,
        };
        let lf = self
            .lexicon
            .query(word, &probe)
            .into_iter()
            .find(|h| h.kind == LexiconMatchKind::Exact)
            .map(|h| h.log_freq);
        if self.exact_probe_cache.len() >= MAX_TOKEN_LEX_CACHE * 4 {
            self.exact_probe_cache.clear();
        }
        self.exact_probe_cache.insert(word.to_string(), lf);
        lf
    }

    /// 漏打空格的两段切分：两侧都必须是常见词，极短一侧（<= 2 字母）只接受高频功能词（a/i/it/me/to…），
    /// 否则几乎任何长词都能被切成"词 + 碎片"。按两侧词频和取前 2 个交给 LM 竞争。
    fn two_way_splits(&mut self, tok: &str, opts: &QueryOptions) -> Vec<(String, String)> {
        let n = tok.len();
        if !tok.is_ascii() || n < 3 {
            return Vec::new();
        }
        let runon = self.profile().runon;
        let keep = self.profile().space_edits.max_two_way_splits;
        let mut cands: Vec<(f32, String, String)> = Vec::new();
        for k in 1..n {
            let (l, r) = tok.split_at(k);
            let min_lf = |w: &str| if w.len() <= 2 { runon.short_part_min_log_freq } else { runon.part_min_log_freq };
            let (Some(fl), Some(fr)) = (self.exact_log_freq(l, opts), self.exact_log_freq(r, opts)) else {
                continue;
            };
            if fl >= min_lf(l) && fr >= min_lf(r) {
                cands.push((fl + fr, l.to_string(), r.to_string()));
            }
        }
        cands.sort_by(|a, b| b.0.total_cmp(&a.0));
        cands.into_iter().take(keep).map(|(_, l, r)| (l, r)).collect()
    }

    /// 空格出错的备选切分（每个备选只含一处空格编辑）：
    /// - 多打空格：相邻两 token 合并后是词（`pr ice -> price`）；
    /// - 空格错位：边界左移 / 右移一个字符后两侧都是词（`wantt o -> want to`、`theatrey ou -> theatre you`）；
    /// - 漏打空格：非词 token 可被连打分词切开（`theone -> the one`）。
    /// 只在至少一侧不是合法词时生成，干净整句不产生任何备选，从结构上保证零误伤。
    fn space_edit_alternatives(
        &mut self,
        tokens: &[(String, usize, usize)],
        opts: &QueryOptions,
    ) -> Vec<Vec<(String, usize, usize)>> {
        let p = *self.profile();
        let exact: Vec<bool> = tokens
            .iter()
            .map(|(t, _, _)| self.is_exact_word(t, opts))
            .collect();
        let mut alts: Vec<Vec<(String, usize, usize)>> = Vec::new();
        let push_alt = |alts: &mut Vec<Vec<(String, usize, usize)>>, i: usize, width: usize, repl: Vec<(String, usize, usize)>| {
            let mut alt = Vec::with_capacity(tokens.len() + 1);
            alt.extend_from_slice(&tokens[..i]);
            alt.extend(repl);
            alt.extend_from_slice(&tokens[i + width..]);
            alts.push(alt);
        };
        for i in 0..tokens.len().saturating_sub(1) {
            if exact[i] && exact[i + 1] {
                continue;
            }
            let (a, sa, ea) = &tokens[i];
            let (b, sb, eb) = &tokens[i + 1];
            if !a.is_ascii() || !b.is_ascii() {
                continue;
            }
            let merged = format!("{a}{b}");
            if self.is_exact_word(&merged, opts) {
                push_alt(&mut alts, i, 2, vec![(merged, *sa, *eb)]);
            }
            if a.len() >= 2 {
                let (a2, moved) = a.split_at(a.len() - 1);
                let b2 = format!("{moved}{b}");
                if self.is_exact_word(a2, opts) && self.is_exact_word(&b2, opts) {
                    push_alt(&mut alts, i, 2, vec![(a2.to_string(), *sa, ea - 1), (b2, ea - 1, *eb)]);
                }
            }
            if b.len() >= 2 {
                let (moved, b2) = b.split_at(1);
                let a2 = format!("{a}{moved}");
                if self.is_exact_word(&a2, opts) && self.is_exact_word(b2, opts) {
                    push_alt(&mut alts, i, 2, vec![(a2, *sa, sb + 1), (b2.to_string(), sb + 1, *eb)]);
                }
            }
        }
        for (i, (t, st, en)) in tokens.iter().enumerate() {
            // 非词，或虽收录但极罕见（如 thankyou）的 token 才尝试拆分
            let rare_exact = exact[i]
                && self.exact_log_freq(t, opts).is_some_and(|lf| lf < p.runon.rare_exact_log_freq);
            if exact[i] && !rare_exact {
                continue;
            }
            for (l, r) in self.two_way_splits(t, opts) {
                let mid = st + l.chars().count();
                push_alt(&mut alts, i, 1, vec![(l, *st, mid), (r, mid, *en)]);
            }
            if t.chars().count() < 4 {
                continue;
            }
            let hits = self.query_lexicon_cached(t, opts);
            if should_expand_runon_token(t, true, &hits, &p.runon) {
                continue; // 已在默认切分中强制拆开
            }
            if let Some(words) = self.lexicon.segment_runon(t) {
                let repl = words.into_iter().map(|w| (w, *st, *en)).collect();
                push_alt(&mut alts, i, 1, repl);
            }
        }
        alts.truncate(p.space_edits.max_alternatives);
        alts
    }

    fn closed_neighbors_cached(&mut self, token: &str) -> Vec<LexiconCandidate> {
        if let Some(cached) = self.neighbor_cache.get(token) {
            return cached.clone();
        }
        let limit = self.profile().confusables.closed_neighbor_query_limit;
        let hits = self.lexicon.closed_token_neighbors(token, limit);
        if self.neighbor_cache.len() >= MAX_TOKEN_LEX_CACHE {
            self.neighbor_cache.clear();
        }
        self.neighbor_cache.insert(token.to_string(), hits.clone());
        hits
    }

    /// 若当前处于句首自动大写上下文（且用户原始输入首字符并非大写），
    /// 在写入 `user.db` 学习前将句首第一个词还原为词典原始大小写（如 "Now" -> "now"），
    /// 同时保留真正的专有名词与第一人称（如 "I"、"Monday"）。
    fn learn(&mut self, learn_text: &str) {
        self.ngram
            .learn_commit(&self.context.before, learn_text, self.privacy);
        self.last_learned_text = (!self.privacy).then(|| learn_text.to_string());
    }

    /// 取走最近一次上屏用于学习的文本（隐私模式下为 `None`）；宿主据此同步 L1 动态热词层，
    /// 保证 L1 与 `user.db` 学到的是同一份句首归一化结果，而非原始上屏文本。
    pub fn take_last_learned_text(&mut self) -> Option<String> {
        self.last_learned_text.take()
    }

    fn normalize_commit_for_learning(&self, text: &str) -> String {
        // 原因注释：句首（输入框起始或句末标点后）的首字母大写只是排版惯例，不是专名证据——
        // 无论是引擎自动大写还是用户手动按 Shift（本机 user.db 中 `When` / `And` / `What` 大量来自后者），
        // 只要该词在主词典中规范为小写，就以小写学习；`I`、`Monday` 等规范大写词不受影响。
        let at_sentence_start =
            self.context.before.trim().is_empty() || context_starts_sentence(&self.context.before);
        if !at_sentence_start {
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

        let p = *self.profile();
        let opts = self.query_options();
        let sentence_start = context_starts_sentence(&self.context.before);
        let has_prev_word_ctx = !sentence_start && last_context_word(&self.context.before).is_some();

        if tokens.len() == 1 {
            let (tok, _, _) = &tokens[0];
            let is_closed_word = self.preedit.ends_with(' ');
            let lex_hits =
                penalize_shorter_corrections(self.query_lexicon_cached(tok, &opts), tok, &p.lexicon);
            let expand_runon = should_expand_runon_token(tok, is_closed_word, &lex_hits, &p.runon);
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

            expand_slot_confusables(
                &self.lexicon,
                tok,
                &mut slot_cands,
                has_prev_word_ctx,
                true,
                &p.confusables,
                self.exact_anchor_bonus(),
            );
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
                let lm_bonus = p.l2.context_weight
                    * self.ngram.score_in_context(&self.context.before, &sc.word);
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
                            (best_single + p.runon.forced_lead).max(p.runon.forced_floor)
                        } else {
                            p.runon.forced_floor
                        }
                    } else if best_single.is_finite() {
                        best_single - p.runon.optional_trail
                    } else {
                        p.runon.optional_fallback
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
            self.place_single_token_verbatim(tok, total_chars, &p.verbatim);
        } else {
            let (slots, first_hits) = self.build_sentence_slots(&tokens, &opts);
            let max = p.lattice.nbest.unwrap_or(self.config.max_candidates);
            let lex_scale = p.lattice.lex_scale;
            let mut sentences = self.ngram.decode_lattice(
                &self.context.before,
                &scale_slot_lex(slots, lex_scale),
                max,
            );
            // 空格可能打错：对可疑词边界生成备选切分，按联合打分 - 空格信道代价与默认切分竞争
            for alt in self.space_edit_alternatives(&tokens, &opts) {
                let (alt_slots, _) = self.build_sentence_slots(&alt, &opts);
                let alt_slots = scale_slot_lex(alt_slots, lex_scale);
                for mut s in self.ngram.decode_lattice(&self.context.before, &alt_slots, max) {
                    s.score -= p.space_edits.penalty;
                    sentences.push(s);
                }
            }
            sentences.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.text.cmp(&b.text)));
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
            if p.verbatim.enabled {
                self.insert_verbatim_sentence(&tokens, sentence_start, total_chars);
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
                    score: hit.score - p.partial.penalty,
                    consumed_chars: *first_end,
                    trailing_space: false,
                });
            }
        }

        self.ensure_raw_and_limit(total_chars);
    }

    /// 整句模式下把用户原样输入（仅句首按惯例首字母大写，其余保留用户大小写）作为整句候选放进固定窗口位。
    ///
    /// Why：L1 会把词典外新词（`ChatGPT`、`UBI`）就近纠成词典词，L2 词格因此永远解码不出原句；
    /// 末位 Raw 兜底又会被 L3 调度按 `CandidateKind::Raw` 过滤掉。若原样句不在 L3 的前
    /// `max_rerank_candidates` 条整句候选里，纠错 L3 就只能在错误改写之间挑选，无法选择"照抄"。
    /// 因此它以 `Sentence` 类型、介于相邻候选之间的分数插在第 `verbatim.slot` 位：不抢 top-1，
    /// L3 的加性重排分数足以在模型确信原句时把它提到首位；仅当 `verbatim_should_lead` 成立时领先。
    fn insert_verbatim_sentence(
        &mut self,
        tokens: &[(String, usize, usize)],
        sentence_start: bool,
        total_chars: usize,
    ) {
        let joined = tokens
            .iter()
            .map(|(t, _, _)| t.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let text = if sentence_start {
            capitalize_first_ascii(&joined)
        } else {
            joined
        };
        let vp = self.profile().verbatim;
        let promote = self.verbatim_should_lead(tokens);
        let slot = if promote { 0 } else { vp.slot };

        // 此时候选全部是整句候选（部分选词稍后追加），按分数降序。
        let existing = self
            .candidates
            .iter()
            .position(|c| c.consumed_chars == total_chars && c.text == text);
        if let Some(idx) = existing {
            if idx <= slot {
                return;
            }
            self.candidates.remove(idx);
        }
        let n = self.candidates.len();
        let pos = slot.min(n);
        let score = if n == 0 {
            0.0
        } else if pos == 0 {
            self.candidates[0].score + vp.lead_margin
        } else if pos < n {
            0.5 * (self.candidates[pos - 1].score + self.candidates[pos].score)
        } else {
            self.candidates[n - 1].score - vp.lead_margin
        };
        self.candidates.insert(
            pos,
            Candidate {
                text,
                kind: CandidateKind::Sentence,
                comment: Some("verbatim".into()),
                score,
                consumed_chars: total_chars,
                trailing_space: self.config.smart_spacing,
            },
        );
    }

    /// 原样句是否直接领先（top-1）：存在"带显式大小写意图的词典外 token"时。
    ///
    /// Why：大小写是唯一几乎不会被错拼制造出来的意图信号。词中大写（`ChatGPT`、`UBI`、`GPUs`）的
    /// 词典外 token，L1 的就近纠错几乎都是改写成无关词典词（`Chapter`、`UP`、`Cups`），保留原样更好；
    /// 纯小写的词典外 token（`roleplay`、`numpy`）与普通错拼无法区分，只进窗口、交给 L3 判断。
    /// - 全大写缩写（可带复数 `s`）一律领先：3–5 位大写串几乎都与某个已知缩写只差一次编辑，
    ///   "附近有大写词典词"不携带信息，而 L1 对缩写错拼本就几乎修不回来；
    /// - 驼峰词若 L1 能召回编辑代价 <= 1.25、且词典规范词形本身含词中大写的近邻（`YouTubr -> YouTube`），
    ///   视为已知驼峰词的错拼，不领先。规范词形必须按小写重新查一次：L1 会把输入的大小写形状
    ///   镜像到候选上（`HVLP -> HELP`），不能直接看候选文本。
    fn verbatim_should_lead(&mut self, tokens: &[(String, usize, usize)]) -> bool {
        tokens.iter().any(|(tok, _, _)| self.token_signals_cased_intent(tok))
    }

    fn token_signals_cased_intent(&mut self, tok: &str) -> bool {
        let word = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if !word.chars().skip(1).any(|c| c.is_ascii_uppercase()) {
            return false;
        }
        if let Some(&cached) = self.verbatim_lead_cache.get(tok) {
            return cached;
        }
        let opts = self.query_options();
        let near_edit_cost = self.profile().verbatim.near_edit_cost;
        let leads = if self.exact_log_freq(word, &opts).is_some() {
            false
        } else if word
            .strip_suffix('s')
            .unwrap_or(word)
            .chars()
            .all(|c| !c.is_ascii_lowercase())
        {
            true
        } else {
            let exact_probe = QueryOptions {
                limit: 3,
                enable_phonetic: false,
                max_edit_cost: 0.0,
                exhaustive_single_edit: false,
            };
            let near: Vec<String> = self
                .query_lexicon_cached(tok, &opts)
                .iter()
                .filter(|h| h.edit_cost <= near_edit_cost)
                .map(|h| h.word.to_ascii_lowercase())
                .collect();
            !near.iter().any(|low| {
                self.lexicon
                    .query(low, &exact_probe)
                    .into_iter()
                    .find(|h| h.kind == LexiconMatchKind::Exact)
                    .is_some_and(|h| h.word.chars().skip(1).any(|c| c.is_ascii_uppercase()))
            })
        };
        if self.verbatim_lead_cache.len() >= MAX_TOKEN_LEX_CACHE {
            self.verbatim_lead_cache.clear();
        }
        self.verbatim_lead_cache.insert(tok.to_string(), leads);
        leads
    }

    /// 单 token 预编辑的原样候选（`verbatim.single_token_*`，单词模式开启）。
    ///
    /// 大小写意图判定与整句原样句共用 `token_signals_cased_intent`（词典词与已知驼峰词错拼不领先）；
    /// 领先时以 `Raw` 类型、比原首选高 `lead_margin` 的分数插到首位。`single_token_slot` 只在输入不是
    /// 词典词时把原样提到指定位次，供纯小写新词（`frontend`、`npm`）一键选回。
    fn place_single_token_verbatim(&mut self, tok: &str, total_chars: usize, vp: &VerbatimProfile) {
        if !vp.single_token_lead && vp.single_token_slot.is_none() {
            return;
        }
        if tok != self.preedit.trim_end() || tok.chars().any(|c| !c.is_ascii_alphabetic()) {
            return;
        }
        let opts = self.query_options();
        if self.exact_log_freq(tok, &opts).is_some() {
            return;
        }
        let slot = if vp.single_token_lead && self.token_signals_cased_intent(tok) {
            0
        } else if let Some(slot) = vp.single_token_slot {
            slot
        } else {
            return;
        };
        if let Some(pos) = self
            .candidates
            .iter()
            .position(|c| c.text == tok && c.consumed_chars == total_chars)
        {
            if pos <= slot {
                return;
            }
            self.candidates.remove(pos);
        }
        let slot = slot.min(self.candidates.len());
        let score = match (slot.checked_sub(1).and_then(|i| self.candidates.get(i)), self.candidates.get(slot)) {
            (_, Some(next)) if slot == 0 => next.score + vp.lead_margin,
            (Some(prev), Some(next)) => (prev.score + next.score) / 2.0,
            (Some(prev), None) => prev.score - vp.lead_margin,
            _ => 0.0,
        };
        self.candidates.insert(
            slot,
            Candidate {
                text: tok.to_string(),
                kind: CandidateKind::Raw,
                comment: Some("verbatim".into()),
                score,
                consumed_chars: total_chars,
                trailing_space: self.config.smart_spacing,
            },
        );
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

/// 见 `LexiconProfile::shorter_correction_penalty`；未开启、输入是词典词或不是合法前缀时原样返回。
fn penalize_shorter_corrections(
    mut hits: Vec<LexiconCandidate>,
    tok: &str,
    lp: &LexiconProfile,
) -> Vec<LexiconCandidate> {
    // 输入本身是词典词时（真词误拼 kit -> it）交给易混词 / 真词近邻机制，不在这里扣分
    if lp.shorter_correction_penalty == 0.0
        || hits.iter().any(|h| h.kind == LexiconMatchKind::Exact)
        || !hits
            .iter()
            .any(|h| h.kind == LexiconMatchKind::Completion && h.edit_cost == 0.0)
    {
        return hits;
    }
    let n = tok.chars().count();
    for h in &mut hits {
        if matches!(
            h.kind,
            LexiconMatchKind::TypoCorrection | LexiconMatchKind::PhoneticCorrection
        ) && h.word.chars().count() < n
        {
            h.score -= lp.shorter_correction_penalty;
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits
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

/// 送入词格前按 profile 缩放槽位词法分；默认 1.0 时原样返回，不做任何浮点运算。
fn scale_slot_lex(mut slots: Vec<LatticeSlot>, scale: f32) -> Vec<LatticeSlot> {
    if scale != 1.0 {
        for slot in &mut slots {
            for c in &mut slot.candidates {
                c.lex_score *= scale;
            }
        }
    }
    slots
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
fn should_expand_runon_token(
    tok: &str,
    is_closed_word: bool,
    hits: &[LexiconCandidate],
    runon: &RunonProfile,
) -> bool {
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
        .any(|h| h.kind == LexiconMatchKind::TypoCorrection && h.edit_cost <= runon.block_typo_edit_cost)
    {
        return false;
    }
    if tok.len() < runon.min_forced_len && !hits.is_empty() {
        return false;
    }
    if !is_closed_word
        && hits
            .iter()
            .any(|h| h.kind == LexiconMatchKind::Completion && h.log_freq >= runon.block_completion_log_freq)
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
    fn sentence_start_capitals_are_learned_lowercase_including_empty_field() {
        let lex = Lexicon::from_word_zipfs(
            vec![("when", 6.4), ("is", 7.2), ("it", 7.3), ("i", 7.0), ("I", 7.0), ("Monday", 5.0), ("linux", 4.0)],
            Vec::new(),
        );
        let mut sess = EngineSession::new(lex, NgramLanguageModel::empty(), NoopNeuralModel);
        let enter = |sess: &mut EngineSession<_, _, _>, s: &str| {
            for ch in s.chars() {
                let _ = sess.handle_key(if ch == ' ' { KeyInput::Space } else { KeyInput::Char(ch) });
            }
            let _ = sess.handle_key(KeyInput::Enter);
            sess.take_last_learned_text()
        };
        // 空输入框首词：用户手动按 Shift 的 When 仍按小写学习
        assert_eq!(enter(&mut sess, "When").as_deref(), Some("when"));
        assert_eq!(sess.ngram().user_store().exact_word_freq("when"), 1);
        assert_eq!(sess.ngram().user_store().exact_word_freq("When"), 0);
        // 句中（上文非句首）保留用户大小写
        sess.set_context("I installed ", "");
        assert_eq!(enter(&mut sess, "Linux").as_deref(), Some("Linux"));
        // 规范大写词（Monday）在句首也不折叠
        sess.set_context("", "");
        assert_eq!(enter(&mut sess, "Monday").as_deref(), Some("Monday"));
        // 隐私模式不导出学习文本
        sess.set_privacy(true);
        assert_eq!(enter(&mut sess, "When"), None);
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
    fn space_edit_alternatives_fix_extra_shifted_and_missing_spaces() {
        let lex = Lexicon::from_word_zipfs(
            vec![
                ("the", 7.7),
                ("best", 6.0),
                ("price", 5.6),
                ("ice", 4.9),
                ("i", 7.0),
                ("want", 6.0),
                ("to", 7.4),
                ("go", 6.2),
                ("book", 5.7),
                ("it", 7.3),
                ("now", 6.4),
                ("is", 7.2),
                ("good", 6.4),
            ],
            Vec::new(),
        );
        let lm = NgramLanguageModel::from_counts(
            vec![("the", 3000), ("best", 300), ("price", 150), ("ice", 60), ("i", 900), ("want", 400),
                 ("to", 1500), ("go", 300), ("book", 200), ("it", 1200), ("now", 400), ("is", 1300),
                 ("good", 300)],
            vec![("the", "best", 200), ("best", "price", 80), ("i", "want", 300), ("want", "to", 280),
                 ("to", "go", 200), ("book", "it", 90), ("it", "now", 60), ("is", "good", 100)],
            vec![("the", "best", "price", 60), ("i", "want", "to", 200), ("want", "to", "go", 150),
                 ("book", "it", "now", 40)],
        );
        let mut sess = EngineSession::new(lex, lm, NoopNeuralModel);
        let top = |sess: &mut EngineSession<_, _, _>, s: &str| {
            sess.reset();
            feed_text(sess, s).candidates[0].text.to_ascii_lowercase()
        };
        assert_eq!(top(&mut sess, "the best pr ice"), "the best price");
        assert_eq!(top(&mut sess, "i wantt o go"), "i want to go");
        assert_eq!(top(&mut sess, "bookit now"), "book it now");
        // 全部为合法词的干净整句不生成任何备选切分
        assert_eq!(top(&mut sess, "it is good"), "it is good");
        let toks = split_char_spans("it is good");
        let opts = QueryOptions::default();
        assert!(sess.space_edit_alternatives(&toks, &opts).is_empty());
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

    #[test]
    fn verbatim_sentence_enters_rerank_window_and_l3_can_choose_it() {
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

        // 纯小写的词典外词 `chatgpt` 与错拼无法区分：L1 就近纠成 `chapter`，原样句只进窗口不领先。
        let lex = Lexicon::from_word_zipfs(
            vec![("i", 7.5), ("asked", 5.5), ("chapter", 5.0), ("about", 6.5), ("it", 7.0)],
            Vec::new(),
        );
        let mut sess = EngineSession::new(lex, NoopNgramModel, MockNeural);
        let mut last = None;
        for (idx, ch) in "i asked chatgpt about it".chars().enumerate() {
            let key = if ch == ' ' {
                KeyInput::Space
            } else {
                KeyInput::Char(ch)
            };
            if let EngineOutput::State(st) = sess.handle_key_with_seq((idx + 1) as u64, key) {
                last = Some(st);
            }
        }
        let st = last.expect("state");
        let full: Vec<&Candidate> = st
            .candidates
            .iter()
            .filter(|c| c.consumed_chars == st.preedit.chars().count() && c.kind != CandidateKind::Raw)
            .collect();
        let pos = full
            .iter()
            .position(|c| c.text == "i asked chatgpt about it")
            .expect("verbatim sentence must be offered");
        assert!(pos > 0, "verbatim must not be forced to top-1: {:?}", st.candidates);
        assert!(pos < 6, "verbatim must sit inside the default L3 rerank window");
        assert_eq!(full[pos].kind, CandidateKind::Sentence);
        assert_eq!(full[pos].comment.as_deref(), Some("verbatim"));
        // 原样句已作为整句候选出现，不再重复追加 Raw 兜底项。
        assert!(!st.candidates.iter().any(|c| c.kind == CandidateKind::Raw));

        let top1 = full[0].text.clone();
        let pushed = sess
            .apply_neural_rerank(
                st.seq,
                &[
                    NeuralCandidate {
                        text: "i asked chatgpt about it".into(),
                        score: 2.0,
                        is_continuation: false,
                    },
                    NeuralCandidate {
                        text: top1.clone(),
                        score: -2.0,
                        is_continuation: false,
                    },
                ],
                &[],
            )
            .expect("apply neural rerank");
        assert_eq!(pushed.candidates[0].text, "i asked chatgpt about it");
        assert!(pushed.candidates.iter().any(|c| c.text == top1));
    }

    #[test]
    fn verbatim_sentence_capitalizes_only_at_sentence_start_and_skips_single_word() {
        let mut sess = make_session();
        sess.set_context("Done. ", "");
        let st = feed_text(&mut sess, "teh qzx");
        assert!(
            st.candidates
            .iter()
            .any(|c| c.text == "Teh qzx" && c.kind == CandidateKind::Sentence),
            "{:?}",
            st.candidates
        );

        let mut single = make_session();
        let st = feed_text(&mut single, "qzx");
        assert!(!st
            .candidates
            .iter()
            .any(|c| c.comment.as_deref() == Some("verbatim")));
    }

    #[test]
    fn verbatim_leads_only_for_cased_out_of_lexicon_tokens() {
        let lex = Lexicon::from_word_zipfs(
            vec![
                ("i", 7.5),
                ("asked", 5.5),
                ("chapter", 5.0),
                ("about", 6.5),
                ("it", 7.0),
                ("up", 6.8),
                ("watched", 5.2),
                ("YouTube", 4.7),
                ("cups", 4.5),
            ],
            Vec::new(),
        );
        let top1 = |input: &str| {
            let mut sess = EngineSession::new(lex.clone(), NoopNgramModel, NoopNeuralModel);
            let st = feed_text(&mut sess, input);
            st.candidates[0].text.clone()
        };
        // 驼峰 / 全大写 / 复数缩写的词典外 token：大小写即意图，原样句领先。
        assert_eq!(top1("i asked ChatGPT about it"), "i asked ChatGPT about it");
        assert_eq!(top1("i asked UBI about it"), "i asked UBI about it");
        assert_eq!(top1("i asked GPUs about it"), "i asked GPUs about it");
        // 已知驼峰词的错拼仍交给 L1 纠正。
        assert_eq!(top1("i watched YouTubr about it"), "i watched YouTube about it");
        // 纯小写的词典外 token 不领先。
        assert_ne!(top1("i asked chatgpt about it"), "i asked chatgpt about it");
    }

    #[test]
    fn word_mode_keeps_exact_word_against_adjacent_key_confusable() {
        // fast/vast 只差 f/v 直接邻键替换；L2 是与词频无关的 PMI 提升，罕见词 vast 的偶见搭配
        // ("type vast"、"the vast") 会拿到比 fast 更高的提升，不得因此翻掉用户已敲出的 fast。
        let lex = Lexicon::from_word_zipfs(
            vec![
                ("fast", 5.1),
                ("vast", 4.5),
                ("type", 5.0),
                ("to", 7.4),
                ("the", 7.7),
                ("from", 6.8),
                ("form", 5.8),
                ("fill", 5.4),
                ("out", 6.2),
            ],
            Vec::new(),
        );
        let lm = NgramLanguageModel::from_counts(
            vec![
                ("fast", 120),
                ("vast", 25),
                ("type", 110),
                ("to", 1500),
                ("the", 2000),
                ("from", 600),
                ("form", 90),
                ("fill", 60),
                ("out", 300),
            ],
            vec![
                ("to", "type", 40),
                ("type", "fast", 2),
                ("type", "vast", 2),
                ("the", "fast", 4),
                ("the", "vast", 20),
                ("out", "the", 80),
                ("the", "form", 60),
            ],
            vec![
                ("to", "type", "vast", 2),
                ("fill", "out", "the", 50),
                ("out", "the", "form", 50),
            ],
        );
        let mut sess = EngineSession::new(lex, lm, NoopNeuralModel);
        for (seq, ctx) in [(10, "type "), (20, "to type "), (30, "the ")] {
            sess.reset_with_seq(seq);
            sess.set_context(ctx, "");
            let st = feed_text(&mut sess, "fast");
            assert_eq!(st.candidates[0].text, "fast", "ctx={ctx:?} {:?}", st.candidates);
            assert!(
                st.candidates.iter().take(5).any(|c| c.text == "vast"),
                "vast must stay selectable: {:?}",
                st.candidates
            );
        }

        // 读音 / 换位类易混词仍由上下文翻盘。
        sess.reset_with_seq(40);
        sess.set_context("fill out the ", "");
        assert_eq!(feed_text(&mut sess, "from").candidates[0].text, "form");
    }

    fn fast_vast_session() -> EngineSession<Lexicon, NgramLanguageModel, NoopNeuralModel> {
        let lex = Lexicon::from_word_zipfs(
            vec![("fast", 5.1), ("vast", 4.5), ("type", 5.0), ("to", 7.4)],
            Vec::new(),
        );
        let lm = NgramLanguageModel::from_counts(
            vec![("fast", 120), ("vast", 25), ("type", 110), ("to", 1500)],
            vec![("to", "type", 40), ("type", "fast", 2), ("type", "vast", 2)],
            vec![("to", "type", "vast", 2)],
        );
        EngineSession::new(lex, lm, NoopNeuralModel)
    }

    #[test]
    fn default_profiles_select_per_mode_and_sentence_stays_baseline() {
        let mut sess = make_session();
        assert_eq!(*sess.profiles(), ModeProfiles::default());
        assert_eq!(*sess.profile(), SENTENCE_PROFILE);
        sess.set_config(EngineConfig {
            space_commits: true,
            ..EngineConfig::default()
        });
        assert_eq!(*sess.profile(), WORD_PROFILE);
        // 单词模式轨道（task-18）只改 WORD_PROFILE；整句模式保持基准
        assert_eq!(SENTENCE_PROFILE, BASELINE_PROFILE);
    }

    #[test]
    fn space_commits_switches_profile_immediately() {
        // 只放开单词模式的邻键下限：整句模式保持 3.0，fast 留在首位；切到单词模式后同一预编辑立即重算，vast 翻盘。
        let mut sess = fast_vast_session();
        let mut profiles = ModeProfiles::default();
        profiles.word.confusables.adjacent_key_single_token_min_penalty = 0.0;
        sess.set_profiles(profiles);
        sess.set_context("to type ", "");
        let st = feed_text(&mut sess, "fast");
        assert_eq!(st.candidates[0].text, "fast", "{:?}", st.candidates);

        sess.set_config(EngineConfig {
            space_commits: true,
            ..EngineConfig::default()
        });
        assert_eq!(sess.preedit(), "fast");
        assert_eq!(sess.candidates()[0].text, "vast", "{:?}", sess.candidates());

        sess.set_config(EngineConfig::default());
        assert_eq!(sess.candidates()[0].text, "fast", "{:?}", sess.candidates());
    }

    #[test]
    fn profile_offsets_and_l3_gate_take_effect() {
        let mut sess = make_session();
        sess.set_config(EngineConfig {
            space_commits: true,
            ..EngineConfig::default()
        });
        feed_text(&mut sess, "app");
        let before: Vec<String> = sess.candidates().iter().map(|c| c.text.clone()).collect();
        assert!(sess.wants_l3_rerank());

        let mut profiles = ModeProfiles::default();
        profiles.word.lexicon.completion_offset = -100.0;
        profiles.word.l3.min_tokens = 2;
        sess.set_profiles(profiles);
        assert!(!sess.wants_l3_rerank(), "single token below word-mode min_tokens");
        let completion = sess
            .candidates()
            .iter()
            .find(|c| c.kind == CandidateKind::Completion)
            .expect("completions still listed");
        assert!(completion.score < -90.0, "{:?}", sess.candidates());

        sess.set_profiles(ModeProfiles::default());
        let after: Vec<String> = sess.candidates().iter().map(|c| c.text.clone()).collect();
        assert_eq!(before, after);
    }

    fn word_mode<L: LexiconProvider, N: NgramModel, M: NeuralSentenceModel>(
        sess: &mut EngineSession<L, N, M>,
        space_commits: bool,
    ) {
        sess.set_config(EngineConfig {
            space_commits,
            ..EngineConfig::default()
        });
    }

    #[test]
    fn word_mode_keeps_cased_novel_token_verbatim() {
        let lex = Lexicon::from_word_zipfs(
            vec![("chapters", 4.6), ("chapter", 4.9), ("chatting", 4.0), ("lm", 3.0), ("youtube", 4.5)],
            Vec::new(),
        );
        let mut sess = EngineSession::new(lex, NoopNgramModel, NoopNeuralModel);
        word_mode(&mut sess, true);
        for tok in ["ChatGPT", "LLM", "LLMs"] {
            sess.reset();
            let st = feed_text(&mut sess, tok);
            assert_eq!(st.candidates[0].text, tok, "{tok}");
        }
        // 纯小写词典外 token 与大小写只在句首的 token 照常纠错
        sess.reset();
        assert_ne!(feed_text(&mut sess, "chapterz").candidates[0].text, "chapterz");
        // 整句模式单 token 不受影响（原样只在末位兜底）
        word_mode(&mut sess, false);
        sess.reset();
        assert_ne!(feed_text(&mut sess, "ChatGPT").candidates[0].text, "ChatGPT");
    }

    #[test]
    fn word_mode_prefers_completion_over_shorter_correction_of_valid_prefix() {
        let lex = Lexicon::from_word_zipfs(vec![("easier", 4.6), ("is", 7.4), ("easy", 5.3)], Vec::new());
        let mut sess = EngineSession::new(lex, NoopNgramModel, NoopNeuralModel);
        word_mode(&mut sess, true);
        assert_eq!(feed_text(&mut sess, "easie").candidates[0].text, "easier");
        let mut profiles = ModeProfiles::default();
        profiles.word.lexicon.shorter_correction_penalty = 0.0;
        profiles.word.lexicon.phonetic_offset = 0.0;
        sess.set_profiles(profiles);
        let texts: Vec<String> = sess.candidates().iter().map(|c| c.text.clone()).collect();
        assert!(texts.iter().any(|t| t == "is"), "{texts:?}");
    }
}
