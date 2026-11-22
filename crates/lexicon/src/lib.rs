//! L1 英文词法与纠错内核：紧凑前缀树、QWERTY 物理键距矩阵、加权模糊束搜索、
//! Double Metaphone 读音倒排、缩写/大小写恢复与 `ANIMELEX` v1 加载器。

pub mod contractions;
pub mod format;
pub mod keyboard;
pub mod lexicon;
pub mod phonetic;
pub mod trie;

use anime_protocol::CandidateKind;

pub use crate::format::{
    ANIMELEX_MAGIC, ANIMELEX_VERSION, ENTRY_FLAG_CAPITALIZED, ENTRY_FLAG_CONTRACTION,
    ENTRY_FLAG_FROM_SCOWL, FLAG_CASE_PRESERVED, FLAG_COMPACT_QUANTIZED, FLAG_PHONETIC_INDEX,
    LexiconBinaryEntry, LexiconData, LexiconError, MisspellingRecord, load_animelex_file,
    parse_animelex_bytes, zipf_to_log_freq,
};
pub use crate::keyboard::weighted_damerau_levenshtein;
pub use crate::lexicon::Lexicon;
pub use crate::phonetic::{PhoneticIndex, encode_double_metaphone};

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
    use crate::keyboard::{deletion_cost, substitution_cost};

    fn make_test_lexicon() -> Lexicon {
        let words = vec![
            ("the", 7.73),
            ("to", 7.43),
            ("and", 7.41),
            ("I", 7.50),
            ("from", 6.80),
            ("form", 5.80),
            ("its", 6.60),
            ("it's", 6.55),
            ("I'm", 6.40),
            ("don't", 6.60),
            ("can't", 6.20),
            ("won't", 5.90),
            ("apple", 5.20),
            ("apples", 4.10),
            ("apply", 5.05),
            ("beautiful", 5.10),
            ("beautifully", 3.90),
            ("beautify", 3.20),
            ("beauty", 4.90),
            ("algorithm", 4.90),
            ("algorithms", 4.20),
            ("kubernetes", 4.40),
            ("environment", 5.10),
            ("configuration", 4.70),
            ("definitely", 5.30),
            ("receive", 5.05),
            ("occurred", 4.75),
            ("restaurant", 5.25),
            ("pneumonia", 3.90),
            ("psychology", 4.60),
            ("physics", 4.80),
            ("philosophy", 4.70),
            ("knowledge", 5.20),
            ("rhythm", 4.45),
            ("receipt", 4.65),
            ("enough", 5.85),
            ("laugh", 5.15),
            ("schedule", 5.00),
            ("Wednesday", 4.80),
            ("subtle", 4.30),
            ("rareword", 2.10),
        ];
        let misspellings = vec![MisspellingRecord {
            typo: "definately".into(),
            target: "definitely".into(),
            weight: 1.0,
        }];
        Lexicon::from_word_zipfs(words, misspellings)
    }

    fn build_synthetic_animelex() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(ANIMELEX_MAGIC);
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&FLAG_CASE_PRESERVED.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes()); // word_count
        buf.extend_from_slice(&1u32.to_le_bytes()); // mis_count
        buf.extend_from_slice(&0u64.to_le_bytes()); // reserved

        for (w, zx, rank, ef) in [("I", 750u16, 1u32, 5u8), ("apple", 520u16, 2u32, 4u8)] {
            buf.extend_from_slice(&(w.len() as u16).to_le_bytes());
            buf.extend_from_slice(w.as_bytes());
            buf.extend_from_slice(&zx.to_le_bytes());
            buf.extend_from_slice(&rank.to_le_bytes());
            buf.push(ef);
            buf.push(0u8); // phonetic_len
        }

        buf.extend_from_slice(&(5u16).to_le_bytes());
        buf.extend_from_slice(b"appie");
        buf.extend_from_slice(&(5u16).to_le_bytes());
        buf.extend_from_slice(b"apple");
        buf.extend_from_slice(&95u16.to_le_bytes());
        buf
    }

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

    #[test]
    fn animelex_binary_parses_and_rejects_corrupt_inputs() {
        let bytes = build_synthetic_animelex();
        let lex = Lexicon::from_animelex_bytes(&bytes).expect("valid ANIMELEX bytes");
        assert_eq!(lex.len(), 2);
        assert_eq!(lex.misspelling_count(), 1);
        let hits = lex.query("appie", &QueryOptions::default());
        assert_eq!(hits[0].word, "apple");

        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            parse_animelex_bytes(&bad_magic),
            Err(LexiconError::InvalidHeader(_))
        ));

        let mut bad_ver = bytes.clone();
        bad_ver[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            parse_animelex_bytes(&bad_ver),
            Err(LexiconError::UnsupportedVersion(2))
        ));

        assert!(matches!(
            parse_animelex_bytes(&bytes[..bytes.len() - 3]),
            Err(LexiconError::Truncated(_))
        ));

        let mut trailing = bytes;
        trailing.push(0);
        assert!(matches!(
            parse_animelex_bytes(&trailing),
            Err(LexiconError::TrailingBytes(1))
        ));
    }

    #[test]
    fn keyboard_geometry_models_physical_neighbors() {
        assert!(substitution_cost(b'i', b'l') < substitution_cost(b'i', b'z'));
        assert!(substitution_cost(b'a', b's') < substitution_cost(b'a', b'p'));
        assert!(deletion_cost(b'p', Some(b'p')) < deletion_cost(b'k', Some(b'a')));
        assert!(
            weighted_damerau_levenshtein("appie", "apple")
                < weighted_damerau_levenshtein("appqe", "apple")
        );
    }

    #[test]
    fn double_metaphone_handles_silent_letters_and_phonetic_clusters() {
        assert_eq!(
            encode_double_metaphone("pneumonia").0,
            encode_double_metaphone("numonia").0
        );
        assert_eq!(
            encode_double_metaphone("psychology").0,
            encode_double_metaphone("sikology").0
        );
        assert_eq!(
            encode_double_metaphone("physics").0,
            encode_double_metaphone("fisics").0
        );
        assert_eq!(
            encode_double_metaphone("philosophy").0,
            encode_double_metaphone("filosofy").0
        );
        assert_eq!(
            encode_double_metaphone("knowledge").0,
            encode_double_metaphone("nolege").0
        );
        assert_eq!(
            encode_double_metaphone("rhythm").0,
            encode_double_metaphone("rythm").0
        );
        assert_eq!(
            encode_double_metaphone("receipt").0,
            encode_double_metaphone("reseet").0
        );
        assert_eq!(
            encode_double_metaphone("enough").0,
            encode_double_metaphone("enuf").0
        );
        assert_eq!(
            encode_double_metaphone("laugh").0,
            encode_double_metaphone("laff").0
        );
    }

    #[test]
    fn lexicon_handles_exact_prefix_typos_phonetic_and_contractions() {
        let lex = make_test_lexicon();
        let opts = QueryOptions::default();

        // 1. 精确防误纠（包含低频精确词与高频近邻词 form/from）
        let form = lex.query("form", &opts);
        assert_eq!(form[0].word, "form");
        assert_eq!(form[0].kind, LexiconMatchKind::Exact);

        let rare = lex.query("rareword", &opts);
        assert_eq!(rare[0].word, "rareword");
        assert_eq!(rare[0].kind, LexiconMatchKind::Exact);

        // 2. 前缀补全
        assert_eq!(lex.query("algor", &opts)[0].word, "algorithm");
        assert_eq!(lex.query("kubern", &opts)[0].word, "kubernetes");

        // 3. QWERTY 邻键 / 换位 / 漏打纠错
        assert_eq!(lex.query("appie", &opts)[0].word, "apple");
        assert_eq!(lex.query("teh", &opts)[0].word, "the");
        assert_eq!(lex.query("recieve", &opts)[0].word, "receive");
        assert_eq!(lex.query("occured", &opts)[0].word, "occurred");

        // 4. 前部错拼 + 截断前缀补全
        assert_eq!(lex.query("beutif", &opts)[0].word, "beautiful");
        assert_eq!(lex.query("enviorn", &opts)[0].word, "environment");
        assert_eq!(lex.query("conifg", &opts)[0].word, "configuration");

        // 5. 重度读音错拼 + 错拼表直达
        assert_eq!(lex.query("numonia", &opts)[0].word, "pneumonia");
        assert_eq!(lex.query("sikology", &opts)[0].word, "psychology");
        assert_eq!(lex.query("definately", &opts)[0].word, "definitely");
        assert_eq!(lex.query("restrant", &opts)[0].word, "restaurant");
        assert_eq!(lex.query("wendsday", &opts)[0].word, "Wednesday");

        // 6. 缩写与大小写恢复（及碰撞缩写不劫持真词）
        assert_eq!(lex.query("i", &opts)[0].word, "I");
        assert_eq!(lex.query("im", &opts)[0].word, "I'm");
        assert_eq!(lex.query("dont", &opts)[0].word, "don't");
        assert_eq!(lex.query("cant", &opts)[0].word, "can't");
        assert_eq!(lex.query("wont", &opts)[0].word, "won't");

        let its_hits = lex.query("its", &opts);
        assert_eq!(its_hits[0].word, "its");
        assert!(its_hits.iter().any(|c| c.word == "it's"));

        // 首字母大写输入继承
        assert_eq!(lex.query("Appie", &opts)[0].word, "Apple");
    }
}
