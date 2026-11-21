//! 平台无关英文输入法会话状态机：管理预编辑区、多路候选融合、数字选词、部分选词与智能空格。

use anime_lexicon::{LexiconProvider, QueryOptions};
use anime_lm_ngram::{LatticeSlot, NgramModel, SlotCandidate};
use anime_lm_neural::NeuralSentenceModel;
use anime_protocol::{
    Candidate, CandidateKind, CommitAction, ContextText, EngineConfig, KeyInput, PreeditState,
};

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
    last_auto_space: bool,
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
            last_auto_space: false,
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: EngineConfig) {
        self.config = config;
        self.recompute_candidates();
    }

    pub fn set_context(&mut self, before: impl Into<String>, after: impl Into<String>) {
        self.context = ContextText {
            before: before.into(),
            after: after.into(),
        };
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
        self.seq = self.seq.saturating_add(1);
        self.preedit.clear();
        self.candidates.clear();
        self.last_auto_space = false;
        self.current_state(false)
    }

    pub fn handle_key(&mut self, key: KeyInput) -> EngineOutput {
        self.seq = self.seq.saturating_add(1);
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
                    self.last_auto_space = false;
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
                    self.last_auto_space = false;
                    EngineOutput::State(self.current_state(false))
                }
            }
            KeyInput::Digit(d) => {
                if self.preedit.is_empty() {
                    self.last_auto_space = false;
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
        self.seq = self.seq.saturating_add(1);
        if self.preedit.is_empty() || index >= self.candidates.len() {
            return None;
        }
        match self.commit_candidate_at(index) {
            EngineOutput::Commit(action) => Some(action),
            EngineOutput::State(_) => None,
        }
    }

    fn handle_char(&mut self, ch: char) -> EngineOutput {
        if self.preedit.is_empty() {
            if self.config.smart_spacing && self.last_auto_space && is_closing_punct(ch) {
                self.last_auto_space = false;
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
                return EngineOutput::State(self.current_state(true));
            }
        }

        if is_word_char(ch) {
            self.last_auto_space = false;
            self.preedit.push(ch);
            self.recompute_candidates();
            EngineOutput::State(self.current_state(false))
        } else {
            // 正在组合时遇到标点，先透传标点或按未来契约扩展；此处保持可预期状态。
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

        // 默认保留词间空格在预编辑区中，支持 "i wnt to eat an appie" 这类多词组合。
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

        self.ngram
            .learn_commit(&self.context.before, &cand.text, self.privacy);

        let add_space =
            self.config.smart_spacing && cand.trailing_space && remaining.is_empty();
        let mut commit_text = cand.text;
        if add_space {
            commit_text.push(' ');
        }

        self.last_auto_space = add_space;
        self.preedit = remaining.clone();
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
            revision: 0,
            cursor: self.preedit.chars().count(),
            preedit: self.preedit.clone(),
            candidates: self.candidates.clone(),
            passthrough,
            final_l3: true,
        }
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
        let prev_ctx_word = last_context_word(&self.context.before);

        if tokens.len() == 1 {
            let (tok, _, _) = &tokens[0];
            let lex_hits = self.lexicon.query(tok, &opts);
            for hit in lex_hits {
                let lm_bonus = self.ngram.score_word(prev_ctx_word, &hit.word);
                self.candidates.push(Candidate {
                    text: hit.word,
                    kind: hit.kind.into(),
                    comment: None,
                    score: hit.score + lm_bonus,
                    consumed_chars: total_chars,
                    trailing_space: self.config.smart_spacing,
                });
            }
            self.candidates
                .sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.text.cmp(&b.text)));
        } else {
            let mut slots = Vec::with_capacity(tokens.len());
            for (tok, start_char, end_char) in &tokens {
                let hits = self.lexicon.query(tok, &opts);
                let mut slot_cands: Vec<SlotCandidate> = hits
                    .into_iter()
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
                self.candidates.push(Candidate {
                    text: s.text,
                    kind: CandidateKind::Sentence,
                    comment: Some("sentence".into()),
                    score: s.score,
                    consumed_chars: total_chars,
                    trailing_space: self.config.smart_spacing,
                });
            }

            // 同时保留首个词条的部分选词候选，便于用户只确认句首词。
            let (first_tok, _, first_end) = &tokens[0];
            for hit in self.lexicon.query(first_tok, &QueryOptions { limit: 2, ..opts }) {
                self.candidates.push(Candidate {
                    text: hit.word,
                    kind: hit.kind.into(),
                    comment: Some("partial".into()),
                    score: hit.score - 1.0,
                    consumed_chars: *first_end,
                    trailing_space: false,
                });
            }
        }

        if self.config.enable_neural && self.neural.backend_name() != "noop" {
            let cands_text: Vec<String> = self.candidates.iter().map(|c| c.text.clone()).collect();
            let _reranked = self
                .neural
                .rerank_sentences(&self.context.before, &self.preedit, &cands_text);
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
    use anime_lexicon::StaticWordList;
    use anime_lm_neural::NoopNeuralModel;
    use anime_lm_ngram::NoopNgramModel;

    fn make_session() -> EngineSession<StaticWordList, NoopNgramModel, NoopNeuralModel> {
        EngineSession::new(
            StaticWordList::smoke_default(),
            NoopNgramModel,
            NoopNeuralModel,
        )
    }

    fn feed_text(
        sess: &mut EngineSession<StaticWordList, NoopNgramModel, NoopNeuralModel>,
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

        // 紧接着按句点自动回删刚才附加的智能尾空格。
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
}
