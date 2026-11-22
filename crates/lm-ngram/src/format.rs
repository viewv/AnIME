//! `ANIMENGR` v1 紧凑小端序 2/3-gram 二进制语言模型解析与编码器（严格对齐 `scripts/data_pipeline/common.py`）。

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

pub const ANIMENGR_MAGIC: &[u8; 8] = b"ANIMENGR";
pub const ANIMENGR_VERSION: u32 = 1;

pub const SPECIAL_UNK: &str = "<unk>";
pub const SPECIAL_BOS: &str = "<bos>";
pub const SPECIAL_EOS: &str = "<eos>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NgramError {
    Io(String),
    InvalidHeader(&'static str),
    UnsupportedVersion(u32),
    Truncated(&'static str),
    InvalidUtf8(&'static str),
    InvalidWordId(u32),
    InvalidFloat(&'static str),
    TrailingBytes(usize),
    Sqlite(String),
}

impl fmt::Display for NgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "I/O error reading ngram resource: {msg}"),
            Self::InvalidHeader(msg) => write!(f, "invalid ANIMENGR header: {msg}"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported ANIMENGR version {v} (expected {ANIMENGR_VERSION})")
            }
            Self::Truncated(field) => write!(f, "truncated ANIMENGR payload at {field}"),
            Self::InvalidUtf8(field) => write!(f, "invalid UTF-8 in ANIMENGR {field}"),
            Self::InvalidWordId(id) => write!(f, "word_id {id} out of vocab bounds"),
            Self::InvalidFloat(field) => write!(f, "non-finite float in ANIMENGR {field}"),
            Self::TrailingBytes(n) => write!(f, "unexpected {n} trailing bytes in ANIMENGR"),
            Self::Sqlite(msg) => write!(f, "SQLite user.db error: {msg}"),
        }
    }
}

impl std::error::Error for NgramError {}

impl From<io::Error> for NgramError {
    fn from(err: io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnigramRecord {
    pub word_id: u32,
    pub log10_p: f32,
    pub backoff: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BigramRecord {
    pub w1: u32,
    pub w2: u32,
    pub log10_p: f32,
    pub backoff: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrigramRecord {
    pub w1: u32,
    pub w2: u32,
    pub w3: u32,
    pub log10_p: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NgramData {
    pub flags: u32,
    pub vocab: Vec<String>,
    pub unigrams: Vec<UnigramRecord>,
    pub bigrams: Vec<BigramRecord>,
    pub trigrams: Vec<TrigramRecord>,
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

    fn read_bytes(&mut self, len: usize, ctx: &'static str) -> Result<&'a [u8], NgramError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(NgramError::Truncated(ctx))?;
        if end > self.buf.len() {
            return Err(NgramError::Truncated(ctx));
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn read_u16_le(&mut self, ctx: &'static str) -> Result<u16, NgramError> {
        let b = self.read_bytes(2, ctx)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn read_u32_le(&mut self, ctx: &'static str) -> Result<u32, NgramError> {
        let b = self.read_bytes(4, ctx)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_f32_le(&mut self, ctx: &'static str) -> Result<f32, NgramError> {
        let b = self.read_bytes(4, ctx)?;
        let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        if !v.is_finite() {
            return Err(NgramError::InvalidFloat(ctx));
        }
        Ok(v)
    }

    fn read_utf8(&mut self, len: usize, ctx: &'static str) -> Result<String, NgramError> {
        let bytes = self.read_bytes(len, ctx)?;
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|_| NgramError::InvalidUtf8(ctx))
    }
}

pub fn load_animengr_file(path: &Path) -> Result<NgramData, NgramError> {
    let bytes = fs::read(path)?;
    parse_animengr_bytes(&bytes)
}

pub fn parse_animengr_bytes(bytes: &[u8]) -> Result<NgramData, NgramError> {
    if bytes.len() < 16 {
        return Err(NgramError::InvalidHeader("file shorter than 16 bytes"));
    }
    let mut cur = Cursor::new(bytes);
    let magic = cur.read_bytes(8, "magic")?;
    if magic != ANIMENGR_MAGIC {
        return Err(NgramError::InvalidHeader(
            "magic mismatch (expected ANIMENGR)",
        ));
    }
    let version = cur.read_u32_le("version")?;
    if version != ANIMENGR_VERSION {
        return Err(NgramError::UnsupportedVersion(version));
    }
    let flags = cur.read_u32_le("flags")?;

    let v_cnt = cur.read_u32_le("vocab_count")? as usize;
    let u_cnt = cur.read_u32_le("unigram_count")? as usize;
    let b_cnt = cur.read_u32_le("bigram_count")? as usize;
    let t_cnt = cur.read_u32_le("trigram_count")? as usize;

    if v_cnt.checked_mul(2).is_none_or(|min_b| min_b > cur.remaining()) {
        return Err(NgramError::Truncated("vocab_count exceeds payload"));
    }
    let mut vocab = Vec::with_capacity(v_cnt);
    for _ in 0..v_cnt {
        let len = cur.read_u16_le("vocab_len")? as usize;
        vocab.push(cur.read_utf8(len, "vocab_token")?);
    }

    let tables_bytes = u_cnt
        .checked_mul(12)
        .and_then(|u| b_cnt.checked_mul(16).and_then(|b| u.checked_add(b)))
        .and_then(|ub| t_cnt.checked_mul(16).and_then(|t| ub.checked_add(t)))
        .ok_or(NgramError::Truncated("n-gram table counts overflow"))?;
    if tables_bytes > cur.remaining() {
        return Err(NgramError::Truncated("n-gram tables exceed payload"));
    }

    let check_id = |id: u32| -> Result<(), NgramError> {
        if (id as usize) < vocab.len() {
            Ok(())
        } else {
            Err(NgramError::InvalidWordId(id))
        }
    };

    let mut unigrams = Vec::with_capacity(u_cnt);
    for _ in 0..u_cnt {
        let word_id = cur.read_u32_le("unigram.word_id")?;
        check_id(word_id)?;
        let log10_p = cur.read_f32_le("unigram.log10_p")?;
        let backoff = cur.read_f32_le("unigram.backoff")?;
        unigrams.push(UnigramRecord {
            word_id,
            log10_p,
            backoff,
        });
    }

    let mut bigrams = Vec::with_capacity(b_cnt);
    for _ in 0..b_cnt {
        let w1 = cur.read_u32_le("bigram.w1")?;
        let w2 = cur.read_u32_le("bigram.w2")?;
        check_id(w1)?;
        check_id(w2)?;
        let log10_p = cur.read_f32_le("bigram.log10_p")?;
        let backoff = cur.read_f32_le("bigram.backoff")?;
        bigrams.push(BigramRecord {
            w1,
            w2,
            log10_p,
            backoff,
        });
    }

    let mut trigrams = Vec::with_capacity(t_cnt);
    for _ in 0..t_cnt {
        let w1 = cur.read_u32_le("trigram.w1")?;
        let w2 = cur.read_u32_le("trigram.w2")?;
        let w3 = cur.read_u32_le("trigram.w3")?;
        check_id(w1)?;
        check_id(w2)?;
        check_id(w3)?;
        let log10_p = cur.read_f32_le("trigram.log10_p")?;
        trigrams.push(TrigramRecord {
            w1,
            w2,
            w3,
            log10_p,
        });
    }

    if cur.remaining() != 0 {
        return Err(NgramError::TrailingBytes(cur.remaining()));
    }

    Ok(NgramData {
        flags,
        vocab,
        unigrams,
        bigrams,
        trigrams,
    })
}

pub fn encode_animengr_bytes(data: &NgramData) -> Result<Vec<u8>, NgramError> {
    let v_cnt = u32::try_from(data.vocab.len())
        .map_err(|_| NgramError::Truncated("vocab_count exceeds u32"))?;
    let u_cnt = u32::try_from(data.unigrams.len())
        .map_err(|_| NgramError::Truncated("unigram_count exceeds u32"))?;
    let b_cnt = u32::try_from(data.bigrams.len())
        .map_err(|_| NgramError::Truncated("bigram_count exceeds u32"))?;
    let t_cnt = u32::try_from(data.trigrams.len())
        .map_err(|_| NgramError::Truncated("trigram_count exceeds u32"))?;

    let mut unigrams = data.unigrams.clone();
    unigrams.sort_unstable_by_key(|r| r.word_id);
    let mut bigrams = data.bigrams.clone();
    bigrams.sort_unstable_by_key(|r| (r.w1, r.w2));
    let mut trigrams = data.trigrams.clone();
    trigrams.sort_unstable_by_key(|r| (r.w1, r.w2, r.w3));

    let mut out = Vec::with_capacity(
        32 + data.vocab.len() * 8
            + unigrams.len() * 12
            + bigrams.len() * 16
            + trigrams.len() * 16,
    );
    out.extend_from_slice(ANIMENGR_MAGIC);
    out.extend_from_slice(&ANIMENGR_VERSION.to_le_bytes());
    out.extend_from_slice(&data.flags.to_le_bytes());
    out.extend_from_slice(&v_cnt.to_le_bytes());
    out.extend_from_slice(&u_cnt.to_le_bytes());
    out.extend_from_slice(&b_cnt.to_le_bytes());
    out.extend_from_slice(&t_cnt.to_le_bytes());

    for tok in &data.vocab {
        let len = u16::try_from(tok.len())
            .map_err(|_| NgramError::Truncated("vocab token exceeds u16"))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(tok.as_bytes());
    }

    let check_id = |id: u32| -> Result<(), NgramError> {
        if (id as usize) < data.vocab.len() {
            Ok(())
        } else {
            Err(NgramError::InvalidWordId(id))
        }
    };

    for u in &unigrams {
        check_id(u.word_id)?;
        if !u.log10_p.is_finite() || !u.backoff.is_finite() {
            return Err(NgramError::InvalidFloat("unigram"));
        }
        out.extend_from_slice(&u.word_id.to_le_bytes());
        out.extend_from_slice(&u.log10_p.to_le_bytes());
        out.extend_from_slice(&u.backoff.to_le_bytes());
    }

    for b in &bigrams {
        check_id(b.w1)?;
        check_id(b.w2)?;
        if !b.log10_p.is_finite() || !b.backoff.is_finite() {
            return Err(NgramError::InvalidFloat("bigram"));
        }
        out.extend_from_slice(&b.w1.to_le_bytes());
        out.extend_from_slice(&b.w2.to_le_bytes());
        out.extend_from_slice(&b.log10_p.to_le_bytes());
        out.extend_from_slice(&b.backoff.to_le_bytes());
    }

    for t in &trigrams {
        check_id(t.w1)?;
        check_id(t.w2)?;
        check_id(t.w3)?;
        if !t.log10_p.is_finite() {
            return Err(NgramError::InvalidFloat("trigram"));
        }
        out.extend_from_slice(&t.w1.to_le_bytes());
        out.extend_from_slice(&t.w2.to_le_bytes());
        out.extend_from_slice(&t.w3.to_le_bytes());
        out.extend_from_slice(&t.log10_p.to_le_bytes());
    }

    Ok(out)
}
