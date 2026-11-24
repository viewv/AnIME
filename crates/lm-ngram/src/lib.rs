//! L2 统计语言模型、Top-K 二阶（3-gram）Beam Viterbi 词格解码器与 SQLite `user.db` 本机学习引擎。

pub mod format;
pub mod model;
pub mod user_db;
pub mod viterbi;

use std::sync::Arc;

pub use crate::format::{
    ANIMENGR_MAGIC, ANIMENGR_VERSION, BigramRecord, NgramData, NgramError, SPECIAL_BOS,
    SPECIAL_EOS, SPECIAL_UNK, TrigramRecord, UnigramRecord, encode_animengr_bytes,
    load_animengr_file, parse_animengr_bytes,
};
pub use crate::model::NgramLanguageModel;
pub use crate::user_db::UserStore;
use crate::user_db::{context_prev_word_lower, tokenize_words};
use crate::viterbi::decode_with_beam_viterbi;

#[derive(Debug, Clone, PartialEq)]
pub struct SlotCandidate {
    pub word: String,
    pub lex_score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LatticeSlot {
    pub raw_token: String,
    pub start_char: usize,
    pub end_char: usize,
    pub candidates: Vec<SlotCandidate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedSentence {
    pub text: String,
    pub score: f32,
    pub word_count: usize,
}

pub trait NgramModel: Send + Sync {
    fn score_word(&self, prev_word: Option<&str>, word: &str) -> f32;

    fn score_in_context(&self, context_before: &str, word: &str) -> f32 {
        let prev = context_prev_word_lower(context_before);
        self.score_word(prev.as_deref(), word)
    }

    fn decode_lattice(
        &self,
        context_before: &str,
        slots: &[LatticeSlot],
        limit: usize,
    ) -> Vec<DecodedSentence>;

    fn learn_commit(&self, context_before: &str, committed_text: &str, privacy: bool);
}

impl<T: NgramModel + ?Sized> NgramModel for Arc<T> {
    fn score_word(&self, prev_word: Option<&str>, word: &str) -> f32 {
        (**self).score_word(prev_word, word)
    }

    fn score_in_context(&self, context_before: &str, word: &str) -> f32 {
        (**self).score_in_context(context_before, word)
    }

    fn decode_lattice(
        &self,
        context_before: &str,
        slots: &[LatticeSlot],
        limit: usize,
    ) -> Vec<DecodedSentence> {
        (**self).decode_lattice(context_before, slots, limit)
    }

    fn learn_commit(&self, context_before: &str, committed_text: &str, privacy: bool) {
        (**self).learn_commit(context_before, committed_text, privacy)
    }
}

impl NgramModel for NgramLanguageModel {
    fn score_word(&self, prev_word: Option<&str>, word: &str) -> f32 {
        let prev_l = prev_word
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty());
        self.score_with_history(None, prev_l.as_deref(), word)
    }

    fn score_in_context(&self, context_before: &str, word: &str) -> f32 {
        if context_before.trim().is_empty() {
            return self.score_with_history(None, None, word);
        }
        let Some(prev1) = context_prev_word_lower(context_before) else {
            // 非空句末边界（如 "Hello. "）：静态 LM 参考 `<bos>`，个人词表走 unigram boost
            let static_lift = self
                .static_table()
                .map(|t| {
                    crate::model::CONTEXT_LIFT_WEIGHT
                        * t.context_pmi_lift(None, Some(SPECIAL_BOS), &word.trim().to_ascii_lowercase())
                })
                .unwrap_or(0.0);
            return static_lift + self.user_store().word_boost(word);
        };
        let toks = tokenize_words(context_before.trim_end_matches([' ', '\t']));
        let prev2 = if toks.len() >= 2 {
            Some(toks[toks.len() - 2].to_ascii_lowercase())
        } else {
            Some(SPECIAL_BOS.to_string())
        };
        self.score_with_history(prev2.as_deref(), Some(&prev1), word)
    }

    fn decode_lattice(
        &self,
        context_before: &str,
        slots: &[LatticeSlot],
        limit: usize,
    ) -> Vec<DecodedSentence> {
        decode_with_beam_viterbi(self, context_before, slots, limit)
    }

    fn learn_commit(&self, context_before: &str, committed_text: &str, privacy: bool) {
        self.user_store()
            .record_commit(context_before, committed_text, privacy);
    }
}

/// Phase 0 空实现桩：直接取每个词槽的最高分候选或原词拼句，不持久化学习状态。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopNgramModel;

impl NgramModel for NoopNgramModel {
    fn score_word(&self, _prev_word: Option<&str>, _word: &str) -> f32 {
        0.0
    }

    fn decode_lattice(
        &self,
        _context_before: &str,
        slots: &[LatticeSlot],
        limit: usize,
    ) -> Vec<DecodedSentence> {
        if slots.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut words = Vec::with_capacity(slots.len());
        let mut total_score = 0.0_f32;
        for slot in slots {
            if let Some(best) = slot.candidates.first() {
                words.push(best.word.as_str());
                total_score += best.lex_score;
            } else {
                words.push(slot.raw_token.as_str());
            }
        }
        vec![DecodedSentence {
            text: words.join(" "),
            score: total_score,
            word_count: slots.len(),
        }]
    }

    fn learn_commit(&self, _context_before: &str, _committed_text: &str, _privacy: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn make_temp_db_path(label: &str) -> (TempDirGuard, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "anime_ngram_{label}_{}_{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).expect("create temp db dir");
        let db = dir.join("user.db");
        (TempDirGuard(dir), db)
    }

    #[test]
    fn noop_ngram_joins_top_slot_candidates() {
        let model = NoopNgramModel;
        let slots = vec![
            LatticeSlot {
                raw_token: "i".into(),
                start_char: 0,
                end_char: 1,
                candidates: vec![SlotCandidate {
                    word: "I".into(),
                    lex_score: -1.0,
                }],
            },
            LatticeSlot {
                raw_token: "eat".into(),
                start_char: 2,
                end_char: 5,
                candidates: vec![SlotCandidate {
                    word: "eat".into(),
                    lex_score: -2.0,
                }],
            },
        ];
        let out = model.decode_lattice("", &slots, 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "I eat");
        assert_eq!(out[0].word_count, 2);
    }

    #[test]
    fn animengr_binary_roundtrip_and_corruption_checks() {
        let data = NgramData {
            flags: 3,
            vocab: vec![
                SPECIAL_UNK.into(),
                SPECIAL_BOS.into(),
                SPECIAL_EOS.into(),
                "apple".into(),
                "eat".into(),
            ],
            unigrams: vec![
                UnigramRecord { word_id: 0, log10_p: -5.0, backoff: -0.4 },
                UnigramRecord { word_id: 1, log10_p: -1.0, backoff: -0.4 },
                UnigramRecord { word_id: 2, log10_p: -1.0, backoff: -0.4 },
                UnigramRecord { word_id: 3, log10_p: -2.4, backoff: -0.4 },
                UnigramRecord { word_id: 4, log10_p: -2.1, backoff: -0.4 },
            ],
            bigrams: vec![BigramRecord {
                w1: 4,
                w2: 3,
                log10_p: -0.6,
                backoff: -0.4,
            }],
            trigrams: vec![TrigramRecord {
                w1: 1,
                w2: 4,
                w3: 3,
                log10_p: -0.5,
            }],
        };

        let bytes = encode_animengr_bytes(&data).expect("encode ANIMENGR");
        let parsed = parse_animengr_bytes(&bytes).expect("parse ANIMENGR");
        assert_eq!(parsed, data);

        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            parse_animengr_bytes(&bad_magic),
            Err(NgramError::InvalidHeader(_))
        ));

        let mut bad_ver = bytes.clone();
        bad_ver[8..12].copy_from_slice(&9u32.to_le_bytes());
        assert!(matches!(
            parse_animengr_bytes(&bad_ver),
            Err(NgramError::UnsupportedVersion(9))
        ));

        assert!(matches!(
            parse_animengr_bytes(&bytes[..bytes.len() - 4]),
            Err(NgramError::Truncated(_))
        ));

        let mut trailing = bytes;
        trailing.push(0);
        assert!(matches!(
            parse_animengr_bytes(&trailing),
            Err(NgramError::TrailingBytes(1))
        ));
    }

    #[test]
    fn viterbi_trigram_disambiguates_real_words_and_multiword_sentences() {
        let lm = NgramLanguageModel::from_counts(
            vec![
                ("go", 120),
                ("from", 500),
                ("form", 350),
                ("here", 240),
                ("fill", 80),
                ("out", 150),
                ("the", 800),
                ("i", 600),
                ("want", 200),
                ("went", 190),
                ("to", 700),
                ("eat", 120),
                ("an", 400),
                ("apple", 90),
                ("apples", 30),
            ],
            vec![
                ("go", "from", 60),
                ("from", "here", 90),
                ("the", "form", 75),
                ("<bos>", "i", 300),
                ("i", "want", 150),
                ("want", "to", 140),
                ("to", "eat", 90),
                ("eat", "an", 55),
                ("an", "apple", 45),
            ],
            vec![
                ("go", "from", "here", 50),
                ("out", "the", "form", 40),
                ("i", "want", "to", 120),
                ("want", "to", "eat", 70),
                ("eat", "an", "apple", 35),
            ],
        );

        // 1. 整句词格 Viterbi："go form here" -> "go from here"
        let slots = vec![
            LatticeSlot {
                raw_token: "go".into(),
                start_char: 0,
                end_char: 2,
                candidates: vec![SlotCandidate { word: "go".into(), lex_score: 4.1 }],
            },
            LatticeSlot {
                raw_token: "form".into(),
                start_char: 3,
                end_char: 7,
                candidates: vec![
                    SlotCandidate { word: "form".into(), lex_score: 3.3 },
                    SlotCandidate { word: "from".into(), lex_score: 1.5 },
                ],
            },
            LatticeSlot {
                raw_token: "here".into(),
                start_char: 8,
                end_char: 12,
                candidates: vec![SlotCandidate { word: "here".into(), lex_score: 4.5 }],
            },
        ];
        let decoded = lm.decode_lattice("", &slots, 3);
        assert_eq!(decoded[0].text, "go from here");

        // 2. 右侧截断但有前序上下文："fill out the " + "from" -> "form"
        let fill_slots = vec![LatticeSlot {
            raw_token: "from".into(),
            start_char: 0,
            end_char: 4,
            candidates: vec![
                SlotCandidate { word: "from".into(), lex_score: 4.3 },
                SlotCandidate { word: "form".into(), lex_score: 2.5 },
            ],
        }];
        let fill_out = lm.decode_lattice("fill out the ", &fill_slots, 2);
        assert_eq!(fill_out[0].text, "form");

        // 3. 多错拼整句："i wnt to eat an appie" -> "I want to eat an apple"
        let sentence_slots = vec![
            LatticeSlot {
                raw_token: "i".into(),
                start_char: 0,
                end_char: 1,
                candidates: vec![SlotCandidate { word: "I".into(), lex_score: 5.0 }],
            },
            LatticeSlot {
                raw_token: "wnt".into(),
                start_char: 2,
                end_char: 5,
                candidates: vec![
                    SlotCandidate { word: "went".into(), lex_score: -5.0 },
                    SlotCandidate { word: "want".into(), lex_score: -5.1 },
                ],
            },
            LatticeSlot {
                raw_token: "to".into(),
                start_char: 6,
                end_char: 8,
                candidates: vec![SlotCandidate { word: "to".into(), lex_score: 5.0 }],
            },
            LatticeSlot {
                raw_token: "eat".into(),
                start_char: 9,
                end_char: 12,
                candidates: vec![SlotCandidate { word: "eat".into(), lex_score: 4.0 }],
            },
            LatticeSlot {
                raw_token: "an".into(),
                start_char: 13,
                end_char: 15,
                candidates: vec![SlotCandidate { word: "an".into(), lex_score: 4.8 }],
            },
            LatticeSlot {
                raw_token: "appie".into(),
                start_char: 16,
                end_char: 21,
                candidates: vec![
                    SlotCandidate { word: "apple".into(), lex_score: -4.2 },
                    SlotCandidate { word: "apples".into(), lex_score: -5.4 },
                ],
            },
        ];
        let sent_out = lm.decode_lattice("", &sentence_slots, 3);
        assert_eq!(sent_out[0].text, "I want to eat an apple");
    }

    #[test]
    fn sqlite_user_db_persists_wal_and_respects_privacy() {
        let (_guard, db_path) = make_temp_db_path("wal_persist");

        {
            let store = UserStore::open_sqlite(&db_path).expect("open sqlite user.db");
            let lm = NgramLanguageModel::with_user_store(store);

            lm.learn_commit("deploy to ", "Kubernetes", false);
            lm.learn_commit("deploy to ", "Kubernetes", false);
            // privacy = true 时绝不落盘
            lm.learn_commit("secret ", "SuperSecretPassphrase", true);
            lm.flush_user_db();

            assert_eq!(lm.user_store().word_freq("kubernetes"), 2);
            assert_eq!(lm.user_store().bigram_freq("to", "kubernetes"), 2);
            assert_eq!(lm.user_store().word_freq("SuperSecretPassphrase"), 0);
            assert!(lm.score_in_context("deploy to ", "Kubernetes") > 1.0);
        }

        // 二次打开应完整恢复个人词汇频次、大小写与二元搭配，并可预热至 LayeredLexicon
        let reopened = UserStore::open_sqlite(&db_path).expect("reopen sqlite user.db");
        assert_eq!(reopened.word_freq("kubernetes"), 2);
        assert_eq!(reopened.bigram_freq("to", "kubernetes"), 2);
        assert_eq!(reopened.word_freq("SuperSecretPassphrase"), 0);
        let snap = reopened.snapshot_user_words();
        assert_eq!(snap, vec![("Kubernetes".to_string(), 2)]);
    }

    #[test]
    fn sqlite_user_db_counts_exact_casing_forms_without_renaming_rows() {
        let (_guard, db_path) = make_temp_db_path("exact_forms");
        {
            let store = UserStore::open_sqlite(&db_path).expect("open sqlite user.db");
            let lm = NgramLanguageModel::with_user_store(store);
            for _ in 0..3 {
                lm.learn_commit("", "so", false);
            }
            // 一次 Shift 粘连的 SO 不得吞并 so 的频次，也不得把 so 行改名
            lm.learn_commit("", "SO", false);
            lm.learn_commit("", "Linux", false);
            lm.learn_commit("", "linux", false);
            lm.flush_user_db();
            let us = lm.user_store();
            assert_eq!(us.exact_word_freq("so"), 3);
            assert_eq!(us.exact_word_freq("SO"), 1);
            // L2 个性化加成大小写不敏感汇总
            assert_eq!(us.word_freq("so"), 4);
            assert_eq!(us.word_freq("LINUX"), 2);
        }
        let reopened = UserStore::open_sqlite(&db_path).expect("reopen sqlite user.db");
        let mut snap = reopened.snapshot_user_words();
        snap.sort();
        assert_eq!(
            snap,
            vec![
                ("Linux".to_string(), 1),
                ("SO".to_string(), 1),
                ("linux".to_string(), 1),
                ("so".to_string(), 3),
            ]
        );
    }

    #[test]
    fn oov_floor_and_unseen_bigram_cap_prevent_probability_inversion() {
        // 构造真实场景的缩影：<unk> 总质量很高 (-2.1)，而 "tomorrow" 为 -5.2，
        // 同时高频词 "the" 出度 >= 4 且已观测二元 ("the", "form") = -3.3，未观测的 "from" unigram 为 -2.3
        let data = NgramData {
            flags: 0,
            vocab: vec![
                SPECIAL_UNK.into(),
                SPECIAL_BOS.into(),
                SPECIAL_EOS.into(),
                "the".into(),
                "from".into(),
                "form".into(),
                "tomorrow".into(),
                "cat".into(),
                "dog".into(),
                "book".into(),
            ],
            unigrams: vec![
                UnigramRecord { word_id: 0, log10_p: -2.1, backoff: -0.4 },
                UnigramRecord { word_id: 1, log10_p: -1.3, backoff: -0.4 },
                UnigramRecord { word_id: 2, log10_p: -1.3, backoff: -0.4 },
                UnigramRecord { word_id: 3, log10_p: -1.3, backoff: -0.4 },
                UnigramRecord { word_id: 4, log10_p: -2.3, backoff: -0.4 },
                UnigramRecord { word_id: 5, log10_p: -3.7, backoff: -0.4 },
                UnigramRecord { word_id: 6, log10_p: -5.2, backoff: -0.4 },
                UnigramRecord { word_id: 7, log10_p: -3.5, backoff: -0.4 },
                UnigramRecord { word_id: 8, log10_p: -3.6, backoff: -0.4 },
                UnigramRecord { word_id: 9, log10_p: -3.4, backoff: -0.4 },
            ],
            bigrams: vec![
                BigramRecord { w1: 3, w2: 5, log10_p: -3.3, backoff: -0.4 }, // (the, form)
                BigramRecord { w1: 3, w2: 7, log10_p: -2.9, backoff: -0.4 }, // (the, cat)
                BigramRecord { w1: 3, w2: 8, log10_p: -3.1, backoff: -0.4 }, // (the, dog)
                BigramRecord { w1: 3, w2: 9, log10_p: -4.2, backoff: -0.4 }, // (the, book) -> min = -4.2
            ],
            trigrams: Vec::new(),
        };

        let lm = NgramLanguageModel::from_data(data, UserStore::in_memory());

        // 1. OOV 词 ("tomorrow's") 不得借高 <unk> 质量反超真实低频词 ("tomorrow")
        let oov_slots = vec![
            LatticeSlot {
                raw_token: "the".into(),
                start_char: 0,
                end_char: 3,
                candidates: vec![SlotCandidate { word: "the".into(), lex_score: 5.0 }],
            },
            LatticeSlot {
                raw_token: "tomorow".into(),
                start_char: 4,
                end_char: 11,
                candidates: vec![
                    SlotCandidate { word: "tomorrow's".into(), lex_score: -4.8 },
                    SlotCandidate { word: "tomorrow".into(), lex_score: -5.0 },
                ],
            },
        ];
        let oov_out = lm.decode_lattice("", &oov_slots, 2);
        assert_eq!(oov_out[0].text, "the tomorrow");

        // 2. 高频前缀 "the" 后未见二元 "from" 封顶压低，确保已见 ("the", "form") 在上下文得分显著胜出
        assert!(lm.score_in_context("the ", "form") > lm.score_in_context("the ", "from") + 1.4);
    }

    #[test]
    fn beam_prefix_cache_matches_fresh_decode_and_invalidates_on_learning() {
        let build = || {
            NgramLanguageModel::from_counts(
                vec![("i", 900), ("want", 400), ("to", 1500), ("too", 300), ("go", 300), ("home", 200),
                     ("now", 150), ("know", 120)],
                vec![("i", "want", 300), ("want", "to", 280), ("to", "go", 200), ("go", "home", 120),
                     ("go", "now", 30)],
                vec![("i", "want", "to", 200), ("want", "to", "go", 150), ("to", "go", "home", 90)],
            )
        };
        let slot = |raw: &str, cands: &[(&str, f32)]| LatticeSlot {
            raw_token: raw.into(),
            start_char: 0,
            end_char: 0,
            candidates: cands.iter().map(|(w, s)| SlotCandidate { word: (*w).into(), lex_score: *s }).collect(),
        };
        let prefix = vec![
            slot("i", &[("i", 5.0)]),
            slot("want", &[("want", 4.0), ("wane", 1.0)]),
            slot("to", &[("to", 4.5), ("too", 3.8)]),
            slot("go", &[("go", 4.0)]),
        ];
        let mut a = prefix.clone();
        a.push(slot("home", &[("home", 3.0), ("hone", 0.5)]));
        let mut b = prefix.clone();
        b.push(slot("now", &[("now", 3.0), ("know", 2.6)]));
        let mut c = prefix.clone();
        c[2] = slot("too", &[("too", 4.5), ("to", 3.8)]);
        c.push(slot("home", &[("home", 3.0)]));

        // 同一模型连续解码共享前缀的词格（命中缓存）与每次新建模型从头解码逐位一致
        let cached = build();
        for lat in [&a, &b, &a, &c, &prefix, &b] {
            for ctx in ["", "so "] {
                assert_eq!(cached.decode_lattice(ctx, lat, 5), build().decode_lattice(ctx, lat, 5));
            }
        }

        // 学习后旧束失效：缓存模型与同样学习过的新模型结果一致
        let before = cached.decode_lattice("", &b, 5);
        let fresh = build();
        for lm in [&cached, &fresh] {
            for _ in 0..30 {
                lm.learn_commit("", "i want too go now", false);
            }
        }
        for lat in [&a, &b, &c] {
            assert_eq!(cached.decode_lattice("", lat, 5), fresh.decode_lattice("", lat, 5));
        }
        assert_ne!(cached.decode_lattice("", &b, 5), before, "learning must change the decode");
    }
}
