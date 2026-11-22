//! `ANIMELEX` v1 紧凑小端序二进制词典解析器（严格校验 16 字节头部与边界）。

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

pub const ANIMELEX_MAGIC: &[u8; 8] = b"ANIMELEX";
pub const ANIMELEX_VERSION: u32 = 1;
pub const DEFAULT_EXTENSION_ZIPF: f32 = 4.5;

pub const FLAG_CASE_PRESERVED: u32 = 1 << 0;
pub const FLAG_PHONETIC_INDEX: u32 = 1 << 1;
pub const FLAG_COMPACT_QUANTIZED: u32 = 1 << 2;

pub const ENTRY_FLAG_CAPITALIZED: u8 = 1 << 0;
pub const ENTRY_FLAG_CONTRACTION: u8 = 1 << 1;
pub const ENTRY_FLAG_FROM_SCOWL: u8 = 1 << 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexiconError {
    Io(String),
    InvalidHeader(&'static str),
    UnsupportedVersion(u32),
    Truncated(&'static str),
    InvalidUtf8(&'static str),
    TrailingBytes(usize),
    InvalidTextFormat { line: usize, detail: String },
    UnsupportedFormat(String),
}

impl fmt::Display for LexiconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "I/O error reading lexicon: {msg}"),
            Self::InvalidHeader(msg) => write!(f, "invalid ANIMELEX header: {msg}"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported ANIMELEX version {v} (expected {ANIMELEX_VERSION})")
            }
            Self::Truncated(field) => write!(f, "truncated ANIMELEX payload at {field}"),
            Self::InvalidUtf8(field) => write!(f, "invalid UTF-8 in ANIMELEX {field}"),
            Self::TrailingBytes(n) => write!(f, "unexpected {n} trailing bytes in ANIMELEX"),
            Self::InvalidTextFormat { line, detail } => {
                write!(f, "invalid wordlist format at line {line}: {detail}")
            }
            Self::UnsupportedFormat(msg) => write!(f, "unsupported lexicon format: {msg}"),
        }
    }
}

impl std::error::Error for LexiconError {}

impl From<io::Error> for LexiconError {
    fn from(err: io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexiconBinaryEntry {
    pub word: String,
    pub zipf: f32,
    pub freq_rank: u32,
    pub entry_flags: u8,
    pub phonetic: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MisspellingRecord {
    pub typo: String,
    pub target: String,
    pub weight: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexiconData {
    pub flags: u32,
    pub words: Vec<LexiconBinaryEntry>,
    pub misspellings: Vec<MisspellingRecord>,
}

/// 将 wordfreq Zipf（约 0..=9）线性映射为与 `StaticWordList` 对齐的 log10 概率量纲（约 -9..=0）。
#[inline]
pub fn zipf_to_log_freq(zipf: f32) -> f32 {
    zipf - 9.0
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn read_bytes(&mut self, len: usize, ctx: &'static str) -> Result<&'a [u8], LexiconError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(LexiconError::Truncated(ctx))?;
        if end > self.buf.len() {
            return Err(LexiconError::Truncated(ctx));
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn read_u8(&mut self, ctx: &'static str) -> Result<u8, LexiconError> {
        Ok(self.read_bytes(1, ctx)?[0])
    }

    fn read_u16_le(&mut self, ctx: &'static str) -> Result<u16, LexiconError> {
        let b = self.read_bytes(2, ctx)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn read_u32_le(&mut self, ctx: &'static str) -> Result<u32, LexiconError> {
        let b = self.read_bytes(4, ctx)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_u64_le(&mut self, ctx: &'static str) -> Result<u64, LexiconError> {
        let b = self.read_bytes(8, ctx)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn read_utf8(&mut self, len: usize, ctx: &'static str) -> Result<String, LexiconError> {
        let bytes = self.read_bytes(len, ctx)?;
        std::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .map_err(|_| LexiconError::InvalidUtf8(ctx))
    }
}

pub fn load_animelex_file(path: &Path) -> Result<LexiconData, LexiconError> {
    let bytes = fs::read(path)?;
    parse_animelex_bytes(&bytes)
}

pub fn parse_animelex_bytes(bytes: &[u8]) -> Result<LexiconData, LexiconError> {
    if bytes.len() < 16 {
        return Err(LexiconError::InvalidHeader("file shorter than 16 bytes"));
    }
    let mut cur = Cursor::new(bytes);

    let magic = cur.read_bytes(8, "magic")?;
    if magic != ANIMELEX_MAGIC {
        return Err(LexiconError::InvalidHeader("magic mismatch (expected ANIMELEX)"));
    }
    let version = cur.read_u32_le("version")?;
    if version != ANIMELEX_VERSION {
        return Err(LexiconError::UnsupportedVersion(version));
    }
    let flags = cur.read_u32_le("flags")?;

    let word_count = cur.read_u32_le("word_count")? as usize;
    let mis_count = cur.read_u32_le("misspelling_count")? as usize;
    let _reserved = cur.read_u64_le("reserved")?;

    // 防止恶意/损坏计数字段触发超大预分配
    if word_count > cur.remaining() {
        return Err(LexiconError::Truncated("word_count exceeds payload"));
    }
    let mut words = Vec::with_capacity(word_count);
    for _ in 0..word_count {
        let w_len = cur.read_u16_le("word_len")? as usize;
        let word = cur.read_utf8(w_len, "word")?;
        let zipf_x100 = cur.read_u16_le("zipf_x100")?;
        let freq_rank = cur.read_u32_le("freq_rank")?;
        let entry_flags = cur.read_u8("entry_flags")?;
        let p_len = cur.read_u8("phonetic_len")? as usize;
        let phonetic = if p_len > 0 {
            cur.read_utf8(p_len, "phonetic")?
        } else {
            String::new()
        };
        words.push(LexiconBinaryEntry {
            word,
            zipf: f32::from(zipf_x100) / 100.0,
            freq_rank,
            entry_flags,
            phonetic,
        });
    }

    if mis_count > cur.remaining() {
        return Err(LexiconError::Truncated("misspelling_count exceeds payload"));
    }
    let mut misspellings = Vec::with_capacity(mis_count);
    for _ in 0..mis_count {
        let t_len = cur.read_u16_le("typo_len")? as usize;
        let typo = cur.read_utf8(t_len, "typo")?;
        let g_len = cur.read_u16_le("target_len")? as usize;
        let target = cur.read_utf8(g_len, "target")?;
        let weight_x100 = cur.read_u16_le("weight_x100")?;
        misspellings.push(MisspellingRecord {
            typo,
            target,
            weight: f32::from(weight_x100) / 100.0,
        });
    }

    if cur.remaining() != 0 {
        return Err(LexiconError::TrailingBytes(cur.remaining()));
    }

    Ok(LexiconData {
        flags,
        words,
        misspellings,
    })
}

/// 解析纯文本单列词表或 TSV 词表（兼容 `data/compiled/lexicon.tsv` 格式）。
///
/// 支持行格式：`word` 或 `word<TAB>zipf[<TAB>freq_rank[<TAB>entry_flags[...]]]`；
/// 自动剥离 UTF-8 BOM，忽略空行、`#` 注释行及无注释的 `word\tzipf` 表头。
pub fn parse_wordlist_tsv(content: &str, default_zipf: f32) -> Result<LexiconData, LexiconError> {
    if !default_zipf.is_finite() || !(0.0..=9.5).contains(&default_zipf) {
        return Err(LexiconError::InvalidTextFormat {
            line: 0,
            detail: format!("default_zipf {default_zipf} out of finite range [0.0, 9.5]"),
        });
    }

    let stripped = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut words = Vec::new();

    for (idx, raw_line) in stripped.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim_end_matches(&['\r', '\n'][..]);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split('\t').collect();
        let word = parts[0].trim();
        if word.is_empty() {
            return Err(LexiconError::InvalidTextFormat {
                line: line_no,
                detail: "empty word column".to_string(),
            });
        }
        if word.chars().any(char::is_whitespace) {
            return Err(LexiconError::InvalidTextFormat {
                line: line_no,
                detail: format!("word '{word}' contains whitespace"),
            });
        }

        // 跳过首条非注释 `word\tzipf...` 表头行
        if words.is_empty()
            && word.eq_ignore_ascii_case("word")
            && parts
                .get(1)
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("zipf"))
        {
            continue;
        }

        let zipf = match parts.get(1).map(|s| s.trim()) {
            Some(col) if !col.is_empty() && !col.starts_with('#') => {
                let parsed: f32 = col.parse().map_err(|_| LexiconError::InvalidTextFormat {
                    line: line_no,
                    detail: format!("invalid zipf value '{col}'"),
                })?;
                if !parsed.is_finite() || !(0.0..=9.5).contains(&parsed) {
                    return Err(LexiconError::InvalidTextFormat {
                        line: line_no,
                        detail: format!("zipf {parsed} out of finite range [0.0, 9.5]"),
                    });
                }
                parsed
            }
            _ => default_zipf,
        };

        let freq_rank = parts
            .get(2)
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or((words.len() + 1) as u32);

        let entry_flags = parts
            .get(3)
            .and_then(|s| s.trim().parse::<u8>().ok())
            .unwrap_or_else(|| {
                let cap = if word.bytes().any(|b| b.is_ascii_uppercase()) {
                    ENTRY_FLAG_CAPITALIZED
                } else {
                    0
                };
                let ctr = if word.contains('\'') {
                    ENTRY_FLAG_CONTRACTION
                } else {
                    0
                };
                cap | ctr
            });

        words.push(LexiconBinaryEntry {
            word: word.to_string(),
            zipf,
            freq_rank,
            entry_flags,
            phonetic: String::new(),
        });
    }

    Ok(LexiconData {
        flags: FLAG_CASE_PRESERVED,
        words,
        misspellings: Vec::new(),
    })
}

pub fn load_wordlist_file(path: &Path, default_zipf: f32) -> Result<LexiconData, LexiconError> {
    let text = fs::read_to_string(path)?;
    parse_wordlist_tsv(&text, default_zipf)
}
