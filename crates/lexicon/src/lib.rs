//! L1 词级接口与 Phase 0 内存词表桩实现（Task 02 将在此实现模糊 Trie 与读音纠错）。

use anime_protocol::CandidateKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexiconMatchKind {
    Exact,
    Completion,
    TypoCorrection,
    PhoneticCorrection,
}

impl From<LexiconMatchKind> for CandidateKind {
    fn from(kind: LexiconMatchKind) -> Self {
        match kind {
            LexiconMatchKind::Exact => Self::Exact,
            LexiconMatchKind::Completion => Self::Completion,
            LexiconMatchKind::TypoCorrection => Self::TypoCorrection,
            LexiconMatchKind::PhoneticCorrection => Self::PhoneticCorrection,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexiconCandidate {
    pub word: String,
    pub kind: LexiconMatchKind,
    pub edit_cost: f32,
    pub log_freq: f32,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryOptions {
    pub limit: usize,
    pub enable_phonetic: bool,
    pub max_edit_cost: f32,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            limit: 9,
            enable_phonetic: true,
            max_edit_cost: 2.5,
        }
    }
}

pub trait LexiconProvider: Send + Sync {
    fn query(&self, raw: &str, opts: &QueryOptions) -> Vec<LexiconCandidate>;
}

/// Phase 0 最小只读词表桩：仅做精确与前缀匹配，用于验证引擎主链路与测试基线。
#[derive(Debug, Clone)]
pub struct StaticWordList {
    entries: Vec<(String, f32)>,
}

impl StaticWordList {
    pub fn with_words<I, S>(words: I) -> Self
    where
        I: IntoIterator<Item = (S, f32)>,
        S: Into<String>,
    {
        let mut entries: Vec<(String, f32)> =
            words.into_iter().map(|(w, f)| (w.into(), f)).collect();
        entries.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Self { entries }
    }

    pub fn smoke_default() -> Self {
        Self::with_words([
            ("the", -1.2),
            ("to", -1.4),
            ("and", -1.5),
            ("an", -2.1),
            ("I", -1.6),
            ("want", -2.5),
            ("eat", -2.8),
            ("apple", -3.0),
            ("from", -2.2),
            ("here", -2.4),
            ("beautiful", -3.1),
            ("algorithm", -3.6),
            ("hello", -2.9),
            ("world", -2.7),
            ("pneumonia", -4.8),
            ("definitely", -3.4),
            ("receive", -3.3),
        ])
    }
}

impl LexiconProvider for StaticWordList {
    fn query(&self, raw: &str, opts: &QueryOptions) -> Vec<LexiconCandidate> {
        let needle = raw.trim().to_ascii_lowercase();
        if needle.is_empty() || opts.limit == 0 {
            return Vec::new();
        }

        let mut out = Vec::new();
        for (word, log_freq) in &self.entries {
            let lower = word.to_ascii_lowercase();
            let kind = if lower == needle {
                LexiconMatchKind::Exact
            } else if lower.starts_with(&needle) {
                LexiconMatchKind::Completion
            } else {
                continue;
            };
            let exact_bonus = if kind == LexiconMatchKind::Exact {
                2.0
            } else {
                0.0
            };
            out.push(LexiconCandidate {
                word: word.clone(),
                kind,
                edit_cost: 0.0,
                log_freq: *log_freq,
                score: *log_freq + exact_bonus,
            });
        }

        out.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.word.cmp(&b.word)));
        out.truncate(opts.limit);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_wordlist_matches_exact_and_prefix() {
        let lex = StaticWordList::smoke_default();
        let exact = lex.query("apple", &QueryOptions::default());
        assert_eq!(exact.first().map(|c| c.word.as_str()), Some("apple"));
        assert_eq!(exact[0].kind, LexiconMatchKind::Exact);

        let prefix = lex.query("beaut", &QueryOptions::default());
        assert_eq!(prefix.first().map(|c| c.word.as_str()), Some("beautiful"));
        assert_eq!(prefix[0].kind, LexiconMatchKind::Completion);
    }
}
