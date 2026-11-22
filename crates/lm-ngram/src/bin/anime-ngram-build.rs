//! `anime-ngram-build`：离线构建 `ANIMENGR` v1 3-gram 表（插值修正 Kneser-Ney + 计数剪枝 + 评测去泄漏）。
//!
//! 仅依赖 std，面向数亿词级语料：n-gram 计数走"分块排序 → 落盘 → 多路归并"的外部排序，
//! 常驻内存只有词表、分块缓冲与剪枝后保留的二元组。由 `scripts/data_pipeline/build_ngram_kn.py` 调用。
//!
//! 平滑（Chen & Goodman 1998 插值修正 KN）：
//! - 三元组用原始计数；二元组用续接计数 `N1+(• w2 w3)`（有多少种不同前驱），一元组用 `N1+(• w3)`。
//!   续接计数衡量"能接在多少种上下文之后"，避免 "francisco" 这类只跟在 "san" 后的高频词在其他语境虚高。
//! - 每阶三档折扣 `D1 / D2 / D3+` 由该阶计数的 count-of-counts 估计，被折扣的质量按 `γ(h)` 插值给低阶。
//! - 落盘为 ARPA 式回退表示：已存条目写插值后的完整概率；剪枝后按
//!   `α(h) = (1 - Σ_保留 p(w|h)) / (1 - Σ_保留 p_低阶(w|h'))` 重新归一回退权重，保证剪枝后仍是合法分布。
//! - 句首以 `<bos> <bos>` 填充；以 `<bos>` 开头的二元组没有前驱可数，按 KenLM 惯例改用原始计数。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use anime_lm_ngram::format::{
    BigramRecord, NgramData, SPECIAL_BOS, SPECIAL_EOS, SPECIAL_UNK, TrigramRecord, UnigramRecord,
    encode_animengr_bytes,
};

/// 21 位词 ID，三元组恰好打包进一个 u64。
const ID_BITS: u32 = 21;
const MAX_VOCAB: usize = (1 << ID_BITS) - 1;
const ID_MASK: u64 = (1 << ID_BITS) - 1;
const LEAK_NGRAM: usize = 5;
const ARPA_IMPOSSIBLE: f32 = -99.0;

#[derive(Debug)]
struct Args {
    inputs: Vec<String>,
    exclude: Option<PathBuf>,
    keep_vocab: Option<PathBuf>,
    vocab_size: usize,
    max_bigrams: usize,
    max_trigrams: usize,
    min_trigram_count: u32,
    min_bigram_cc: u32,
    chunk_entries: usize,
    tmp_dir: PathBuf,
    out: PathBuf,
    stats: Option<PathBuf>,
}

fn usage() -> ! {
    eprintln!(
        "usage: anime-ngram-build --input <path|-> [--input ...] --out <ngram.animengr.bin> --tmp-dir <dir>\n\
         \x20 [--exclude eval_sentences.txt] [--keep-vocab words.txt] [--vocab-size 250000]\n\
         \x20 [--max-bigrams 5000000] [--max-trigrams 7000000] [--min-trigram-count 2] [--min-bigram-cc 1]\n\
         \x20 [--chunk-entries 40000000] [--stats stats.json]"
    );
    process::exit(2);
}

fn parse_args() -> Args {
    let mut a = Args {
        inputs: Vec::new(),
        exclude: None,
        keep_vocab: None,
        vocab_size: 250_000,
        max_bigrams: 5_000_000,
        max_trigrams: 7_000_000,
        min_trigram_count: 2,
        min_bigram_cc: 1,
        chunk_entries: 40_000_000,
        tmp_dir: PathBuf::new(),
        out: PathBuf::new(),
        stats: None,
    };
    let mut it = env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--input" => a.inputs.push(val()),
            "--exclude" => a.exclude = Some(PathBuf::from(val())),
            "--keep-vocab" => a.keep_vocab = Some(PathBuf::from(val())),
            "--vocab-size" => a.vocab_size = val().parse().unwrap_or_else(|_| usage()),
            "--max-bigrams" => a.max_bigrams = val().parse().unwrap_or_else(|_| usage()),
            "--max-trigrams" => a.max_trigrams = val().parse().unwrap_or_else(|_| usage()),
            "--min-trigram-count" => a.min_trigram_count = val().parse().unwrap_or_else(|_| usage()),
            "--min-bigram-cc" => a.min_bigram_cc = val().parse().unwrap_or_else(|_| usage()),
            "--chunk-entries" => a.chunk_entries = val().parse().unwrap_or_else(|_| usage()),
            "--tmp-dir" => a.tmp_dir = PathBuf::from(val()),
            "--out" => a.out = PathBuf::from(val()),
            "--stats" => a.stats = Some(PathBuf::from(val())),
            _ => usage(),
        }
    }
    if a.inputs.is_empty() || a.out.as_os_str().is_empty() || a.tmp_dir.as_os_str().is_empty() {
        usage();
    }
    a
}

/// 统一归一化：小写 ASCII；字母与撇号成词；数字、标点、其他符号视为句子断点。
/// 输入法预编辑里数字是选词键、标点会直接上屏，跨越它们的 n-gram 在键入时不会出现。
fn normalize_into(line: &str, out: &mut Vec<Vec<String>>) {
    let mut sent: Vec<String> = Vec::new();
    let mut tok = String::new();
    let flush_tok = |tok: &mut String, sent: &mut Vec<String>| {
        let t = tok.trim_matches('\'');
        if !t.is_empty() {
            sent.push(t.to_string());
        }
        tok.clear();
    };
    for ch in line.chars() {
        let c = match ch {
            '\u{2019}' | '\u{2018}' => '\'',
            other => other.to_ascii_lowercase(),
        };
        if c.is_ascii_lowercase() || c == '\'' {
            tok.push(c);
        } else if c == ' ' || c == '\t' || c == '-' {
            flush_tok(&mut tok, &mut sent);
        } else {
            flush_tok(&mut tok, &mut sent);
            if !sent.is_empty() {
                out.push(std::mem::take(&mut sent));
            }
        }
    }
    flush_tok(&mut tok, &mut sent);
    if !sent.is_empty() {
        out.push(sent);
    }
}

struct LeakFilter {
    sentences: HashSet<String>,
    ngrams: HashSet<String>,
}

impl LeakFilter {
    fn load(path: Option<&Path>) -> io::Result<Self> {
        let mut f = Self {
            sentences: HashSet::new(),
            ngrams: HashSet::new(),
        };
        let Some(path) = path else { return Ok(f) };
        let mut buf = Vec::new();
        for line in BufReader::new(File::open(path)?).lines() {
            buf.clear();
            normalize_into(&line?, &mut buf);
            for sent in &buf {
                f.sentences.insert(sent.join(" "));
                for w in sent.windows(LEAK_NGRAM) {
                    f.ngrams.insert(w.join(" "));
                }
            }
        }
        Ok(f)
    }

    fn is_leak(&self, sent: &[String]) -> bool {
        if self.sentences.is_empty() {
            return false;
        }
        if self.sentences.contains(&sent.join(" ")) {
            return true;
        }
        sent.windows(LEAK_NGRAM)
            .any(|w| self.ngrams.contains(&w.join(" ")))
    }
}

fn open_input(spec: &str) -> io::Result<Box<dyn BufRead>> {
    if spec == "-" {
        Ok(Box::new(BufReader::with_capacity(1 << 20, io::stdin())))
    } else {
        Ok(Box::new(BufReader::with_capacity(1 << 20, File::open(spec)?)))
    }
}

/// 外部排序计数器：缓冲 `(key, weight)`，满了排序聚合后落盘为有序 run。
struct RunSorter {
    buf: Vec<(u64, u32)>,
    cap: usize,
    runs: Vec<PathBuf>,
    dir: PathBuf,
    tag: &'static str,
}

impl RunSorter {
    fn new(dir: &Path, tag: &'static str, cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap.min(1 << 24)),
            cap,
            runs: Vec::new(),
            dir: dir.to_path_buf(),
            tag,
        }
    }

    #[inline]
    fn push(&mut self, key: u64, w: u32) -> io::Result<()> {
        self.buf.push((key, w));
        if self.buf.len() >= self.cap {
            self.spill()?;
        }
        Ok(())
    }

    fn spill(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.buf.sort_unstable_by_key(|e| e.0);
        let path = self.dir.join(format!("{}-run{:04}.bin", self.tag, self.runs.len()));
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&path)?);
        let mut cur = self.buf[0];
        for &(k, c) in &self.buf[1..] {
            if k == cur.0 {
                cur.1 = cur.1.saturating_add(c);
            } else {
                write_rec(&mut w, cur)?;
                cur = (k, c);
            }
        }
        write_rec(&mut w, cur)?;
        w.flush()?;
        self.runs.push(path);
        self.buf.clear();
        Ok(())
    }

    /// 多路归并所有 run 为单个有序去重文件。
    fn finish(mut self, out: &Path) -> io::Result<u64> {
        self.spill()?;
        self.buf = Vec::new();
        let mut readers: Vec<RecReader> = self
            .runs
            .iter()
            .map(|p| RecReader::open(p))
            .collect::<io::Result<_>>()?;
        let mut heads: Vec<Option<(u64, u32)>> = Vec::with_capacity(readers.len());
        let mut heap = BinaryHeap::new();
        for (i, r) in readers.iter_mut().enumerate() {
            let h = r.next_rec()?;
            if let Some((k, _)) = h {
                heap.push(Reverse((k, i)));
            }
            heads.push(h);
        }
        let mut w = BufWriter::with_capacity(1 << 20, File::create(out)?);
        let mut cur: Option<(u64, u32)> = None;
        let mut n = 0u64;
        while let Some(Reverse((k, i))) = heap.pop() {
            let (_, c) = heads[i].take().expect("heap head");
            match cur {
                Some((ck, cc)) if ck == k => cur = Some((ck, cc.saturating_add(c))),
                Some(prev) => {
                    write_rec(&mut w, prev)?;
                    n += 1;
                    cur = Some((k, c));
                }
                None => cur = Some((k, c)),
            }
            heads[i] = readers[i].next_rec()?;
            if let Some((nk, _)) = heads[i] {
                heap.push(Reverse((nk, i)));
            }
        }
        if let Some(prev) = cur {
            write_rec(&mut w, prev)?;
            n += 1;
        }
        w.flush()?;
        for p in &self.runs {
            let _ = fs::remove_file(p);
        }
        Ok(n)
    }
}

#[inline]
fn write_rec(w: &mut impl Write, (k, c): (u64, u32)) -> io::Result<()> {
    w.write_all(&k.to_le_bytes())?;
    w.write_all(&c.to_le_bytes())
}

struct RecReader {
    r: BufReader<File>,
}

impl RecReader {
    fn open(p: &Path) -> io::Result<Self> {
        Ok(Self {
            r: BufReader::with_capacity(1 << 20, File::open(p)?),
        })
    }

    fn next_rec(&mut self) -> io::Result<Option<(u64, u32)>> {
        let mut b = [0u8; 12];
        match self.r.read_exact(&mut b) {
            Ok(()) => {
                let k = u64::from_le_bytes(b[..8].try_into().expect("8 bytes"));
                let c = u32::from_le_bytes(b[8..].try_into().expect("4 bytes"));
                Ok(Some((k, c)))
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[inline]
fn pack3(a: u32, b: u32, c: u32) -> u64 {
    (u64::from(a) << (2 * ID_BITS)) | (u64::from(b) << ID_BITS) | u64::from(c)
}

#[inline]
fn unpack3(k: u64) -> (u32, u32, u32) {
    (
        (k >> (2 * ID_BITS)) as u32,
        ((k >> ID_BITS) & ID_MASK) as u32,
        (k & ID_MASK) as u32,
    )
}

#[inline]
fn pack2(a: u32, b: u32) -> u64 {
    (u64::from(a) << ID_BITS) | u64::from(b)
}

#[inline]
fn unpack2(k: u64) -> (u32, u32) {
    ((k >> ID_BITS) as u32, (k & ID_MASK) as u32)
}

/// 修正 KN 三档折扣（Chen & Goodman 式 26），由计数为 1..4 的条目数估计。
#[derive(Debug, Clone, Copy)]
struct Discounts {
    d: [f64; 3],
}

impl Discounts {
    fn estimate(n: [u64; 4]) -> Self {
        let [n1, n2, n3, n4] = n.map(|x| x as f64);
        if n1 <= 0.0 || n2 <= 0.0 || n3 <= 0.0 || n4 <= 0.0 {
            return Self { d: [0.5, 1.0, 1.5] };
        }
        let y = n1 / (n1 + 2.0 * n2);
        let d1 = (1.0 - 2.0 * y * n2 / n1).clamp(0.05, 0.95);
        let d2 = (2.0 - 3.0 * y * n3 / n2).clamp(0.05, 1.95);
        let d3 = (3.0 - 4.0 * y * n4 / n3).clamp(0.05, 2.95);
        Self { d: [d1, d2, d3] }
    }

    #[inline]
    fn of(&self, c: u32) -> f64 {
        match c {
            0 => 0.0,
            1 => self.d[0],
            2 => self.d[1],
            _ => self.d[2],
        }
    }
}

#[inline]
fn coc_add(n: &mut [u64; 4], c: u32) {
    if (1..=4).contains(&c) {
        n[(c - 1) as usize] += 1;
    }
}

/// 从计数直方图（下标 = 计数，末位桶为"及以上"）挑选最小阈值，使保留条目数不超过预算。
fn pick_threshold(hist: &[u64], floor: u32, budget: usize) -> u32 {
    let mut above: u64 = hist.iter().skip(floor as usize).sum();
    let mut k = floor as usize;
    while above > budget as u64 && k + 1 < hist.len() {
        above -= hist[k];
        k += 1;
    }
    k as u32
}

const HIST_BUCKETS: usize = 4096;

#[inline]
fn hist_add(h: &mut [u64], c: u32) {
    h[(c as usize).min(HIST_BUCKETS - 1)] += 1;
}

struct Stats(Vec<(String, String)>);

impl Stats {
    fn put(&mut self, k: &str, v: impl ToString) {
        self.0.push((k.to_string(), v.to_string()));
    }

    fn to_json(&self) -> String {
        let body: Vec<String> = self
            .0
            .iter()
            .map(|(k, v)| {
                let is_num = v.parse::<f64>().is_ok();
                if is_num {
                    format!("  \"{k}\": {v}")
                } else {
                    format!("  \"{k}\": \"{}\"", v.replace('"', "'"))
                }
            })
            .collect();
        format!("{{\n{}\n}}\n", body.join(",\n"))
    }
}

fn run() -> io::Result<()> {
    let args = parse_args();
    fs::create_dir_all(&args.tmp_dir)?;
    let t0 = Instant::now();
    let mut stats = Stats(Vec::new());

    // ---- Pass 1：归一化 + 去泄漏 + 一元计数，同时落盘归一化语料供 Pass 2 复读 ----
    let leak = LeakFilter::load(args.exclude.as_deref())?;
    let corpus_path = args.tmp_dir.join("corpus.norm.txt");
    let mut corpus_w = BufWriter::with_capacity(1 << 22, File::create(&corpus_path)?);
    let mut uni: HashMap<String, u64> = HashMap::new();
    let (mut lines, mut sents, mut leaked, mut tokens) = (0u64, 0u64, 0u64, 0u64);
    let mut buf: Vec<Vec<String>> = Vec::new();
    for spec in &args.inputs {
        let mut r = open_input(spec)?;
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line)? == 0 {
                break;
            }
            lines += 1;
            buf.clear();
            normalize_into(&line, &mut buf);
            for sent in &buf {
                if leak.is_leak(sent) {
                    leaked += 1;
                    continue;
                }
                sents += 1;
                tokens += sent.len() as u64;
                for t in sent {
                    if let Some(c) = uni.get_mut(t.as_str()) {
                        *c += 1;
                    } else {
                        uni.insert(t.clone(), 1);
                    }
                }
                corpus_w.write_all(sent.join(" ").as_bytes())?;
                corpus_w.write_all(b"\n")?;
            }
            if lines % 5_000_000 == 0 {
                eprintln!("[pass1] {lines} lines, {sents} sentences, {leaked} leaked, {:?}", t0.elapsed());
            }
        }
    }
    corpus_w.flush()?;
    drop(corpus_w);
    eprintln!("[pass1] done: {lines} lines, {sents} sentences, {tokens} tokens, {} types, {leaked} leaked", uni.len());
    stats.put("input_lines", lines);
    stats.put("sentences", sents);
    stats.put("tokens", tokens);
    stats.put("types", uni.len());
    stats.put("leak_filtered_sentences", leaked);
    stats.put("leak_reference_sentences", leak.sentences.len());
    stats.put("leak_reference_ngrams", leak.ngrams.len());

    // ---- 词表：高频前 N ∪ 指定保留词（如 L1 词库），三个特殊符号固定在前 ----
    let mut by_freq: Vec<(&String, &u64)> = uni.iter().collect();
    by_freq.sort_unstable_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let mut chosen: HashSet<String> = by_freq
        .iter()
        .take(args.vocab_size)
        .map(|(w, _)| (*w).clone())
        .collect();
    if let Some(kv) = &args.keep_vocab {
        for line in BufReader::new(File::open(kv)?).lines() {
            let w = line?.trim().to_ascii_lowercase();
            if !w.is_empty() && uni.contains_key(&w) {
                chosen.insert(w);
            }
        }
    }
    for sp in [SPECIAL_UNK, SPECIAL_BOS, SPECIAL_EOS] {
        chosen.remove(sp);
    }
    let mut words: Vec<String> = chosen.into_iter().collect();
    words.sort_unstable();
    words.truncate(MAX_VOCAB - 3);
    let mut vocab = vec![SPECIAL_UNK.to_string(), SPECIAL_BOS.to_string(), SPECIAL_EOS.to_string()];
    vocab.extend(words);
    let id_of: HashMap<&str, u32> = vocab.iter().enumerate().map(|(i, w)| (w.as_str(), i as u32)).collect();
    let (unk, bos, eos) = (0u32, 1u32, 2u32);
    let covered: u64 = vocab.iter().skip(3).filter_map(|w| uni.get(w)).sum();
    stats.put("vocab_size", vocab.len());
    stats.put("vocab_token_coverage", format!("{:.6}", covered as f64 / tokens.max(1) as f64));
    drop(by_freq);
    drop(uni);

    // ---- Pass 2：三元组原始计数（外部排序） ----
    let tri_path = args.tmp_dir.join("trigrams.bin");
    {
        let mut sorter = RunSorter::new(&args.tmp_dir, "tri", args.chunk_entries);
        let r = BufReader::with_capacity(1 << 22, File::open(&corpus_path)?);
        let mut ids: Vec<u32> = Vec::new();
        for line in r.lines() {
            let line = line?;
            ids.clear();
            ids.extend([bos, bos]);
            ids.extend(line.split(' ').map(|t| id_of.get(t).copied().unwrap_or(unk)));
            ids.push(eos);
            for w in ids.windows(3) {
                sorter.push(pack3(w[0], w[1], w[2]), 1)?;
            }
        }
        let n = sorter.finish(&tri_path)?;
        eprintln!("[pass2] {n} distinct trigrams, {:?}", t0.elapsed());
        stats.put("trigram_types", n);
    }
    let _ = fs::remove_file(&corpus_path);

    // ---- Pass A：三元组 count-of-counts、剪枝直方图，并发射二元续接计数 ----
    let bi_path = args.tmp_dir.join("bigram_cc.bin");
    let mut tri_coc = [0u64; 4];
    let mut tri_hist = vec![0u64; HIST_BUCKETS];
    {
        let mut sorter = RunSorter::new(&args.tmp_dir, "bi", args.chunk_entries);
        let mut r = RecReader::open(&tri_path)?;
        while let Some((k, c)) = r.next_rec()? {
            let (a, b, cw) = unpack3(k);
            coc_add(&mut tri_coc, c);
            if !(a == bos && b == bos) {
                hist_add(&mut tri_hist, c);
            }
            // 以 <bos> 开头的二元组没有前驱可数，沿用原始计数（KenLM 同款处理）
            let w = if a == bos && b == bos { c } else { 1 };
            sorter.push(pack2(b, cw), w)?;
        }
        let n = sorter.finish(&bi_path)?;
        eprintln!("[passA] {n} bigram types, {:?}", t0.elapsed());
        stats.put("bigram_types", n);
    }
    let k_tri = pick_threshold(&tri_hist, args.min_trigram_count, args.max_trigrams);
    let tri_disc = Discounts::estimate(tri_coc);

    // 需要保留的三元组上下文 (w1,w2)，其二元组必须保留以承载回退权重
    let mut forced_ctx: Vec<u64> = Vec::new();
    {
        let mut r = RecReader::open(&tri_path)?;
        while let Some((k, c)) = r.next_rec()? {
            let (a, b, _) = unpack3(k);
            if c >= k_tri && !(a == bos && b == bos) {
                let ctx = pack2(a, b);
                if forced_ctx.last() != Some(&ctx) {
                    forced_ctx.push(ctx);
                }
            }
        }
    }

    // ---- Pass B：一元续接计数与二元 count-of-counts ----
    let v = vocab.len();
    let mut uni_cc = vec![0u32; v];
    let mut bi_coc = [0u64; 4];
    let mut bi_hist = vec![0u64; HIST_BUCKETS];
    let mut bigram_type_total = 0u64;
    {
        let mut r = RecReader::open(&bi_path)?;
        while let Some((k, c)) = r.next_rec()? {
            let (_, w3) = unpack2(k);
            uni_cc[w3 as usize] = uni_cc[w3 as usize].saturating_add(1);
            coc_add(&mut bi_coc, c);
            hist_add(&mut bi_hist, c);
            bigram_type_total += 1;
        }
    }
    let k_bi = pick_threshold(
        &bi_hist,
        args.min_bigram_cc,
        args.max_bigrams.saturating_sub(forced_ctx.len()),
    );
    let bi_disc = Discounts::estimate(bi_coc);

    // ---- 一元：p(w) = max(N1+(•w) - D, 0) / N1+(••) + γ / |V| ----
    let mut uni_coc = [0u64; 4];
    for &c in &uni_cc {
        coc_add(&mut uni_coc, c);
    }
    let uni_disc = Discounts::estimate(uni_coc);
    let total_cc: f64 = uni_cc.iter().map(|&c| f64::from(c)).sum::<f64>().max(1.0);
    let gamma_uni: f64 = uni_cc.iter().map(|&c| uni_disc.of(c)).sum::<f64>() / total_cc;
    let p_uni: Vec<f64> = uni_cc
        .iter()
        .map(|&c| ((f64::from(c) - uni_disc.of(c)).max(0.0) / total_cc + gamma_uni / v as f64).max(1e-12))
        .collect();
    drop(uni_cc);

    // ---- 二元：按 w2 分组插值，保留 cc >= k_bi 或被三元上下文强制保留的条目 ----
    let mut kept_bi: Vec<(u64, f32, f32)> = Vec::new();
    let mut bi_alpha = vec![1.0f64; v];
    {
        let mut r = RecReader::open(&bi_path)?;
        let mut group: Vec<(u32, u32)> = Vec::new();
        let mut cur_w2: Option<u32> = None;
        let mut forced_i = 0usize;
        let mut flush = |w2: u32, group: &mut Vec<(u32, u32)>, forced_i: &mut usize| {
            let cc_sum: f64 = group.iter().map(|&(_, c)| f64::from(c)).sum();
            let gamma: f64 = group.iter().map(|&(_, c)| bi_disc.of(c)).sum::<f64>() / cc_sum;
            let (mut kept_p, mut kept_lo) = (0.0f64, 0.0f64);
            for &(w3, c) in group.iter() {
                let key = pack2(w2, w3);
                while *forced_i < forced_ctx.len() && forced_ctx[*forced_i] < key {
                    *forced_i += 1;
                }
                let forced = *forced_i < forced_ctx.len() && forced_ctx[*forced_i] == key;
                if c >= k_bi || forced {
                    let p = (f64::from(c) - bi_disc.of(c)).max(0.0) / cc_sum + gamma * p_uni[w3 as usize];
                    kept_p += p;
                    kept_lo += p_uni[w3 as usize];
                    kept_bi.push((key, p.log10() as f32, 0.0));
                }
            }
            bi_alpha[w2 as usize] = renorm_alpha(kept_p, kept_lo, gamma);
            group.clear();
        };
        while let Some((k, c)) = r.next_rec()? {
            let (w2, w3) = unpack2(k);
            if cur_w2 != Some(w2) {
                if let Some(prev) = cur_w2 {
                    flush(prev, &mut group, &mut forced_i);
                }
                cur_w2 = Some(w2);
            }
            group.push((w3, c));
        }
        if let Some(prev) = cur_w2 {
            flush(prev, &mut group, &mut forced_i);
        }
    }
    let _ = fs::remove_file(&bi_path);
    drop(forced_ctx);
    eprintln!("[passB] kept {} bigrams (k_bi={k_bi}), {:?}", kept_bi.len(), t0.elapsed());

    let p_bi = |w2: u32, w3: u32, kept: &[(u64, f32, f32)]| -> f64 {
        let key = pack2(w2, w3);
        match kept.binary_search_by_key(&key, |e| e.0) {
            Ok(i) => 10f64.powf(f64::from(kept[i].1)),
            Err(_) => bi_alpha[w2 as usize] * p_uni[w3 as usize],
        }
    };

    // ---- 三元：按 (w1,w2) 分组插值，保留 c >= k_tri，并回填上下文二元组的回退权重 ----
    let mut trigrams: Vec<TrigramRecord> = Vec::new();
    {
        let mut r = RecReader::open(&tri_path)?;
        let mut group: Vec<(u32, u32)> = Vec::new();
        let mut cur: Option<(u32, u32)> = None;
        let mut flush = |ctx: (u32, u32), group: &mut Vec<(u32, u32)>, kept_bi: &mut Vec<(u64, f32, f32)>| {
            let (a, b) = ctx;
            if a == bos && b == bos {
                group.clear();
                return;
            }
            let c_sum: f64 = group.iter().map(|&(_, c)| f64::from(c)).sum();
            let gamma: f64 = group.iter().map(|&(_, c)| tri_disc.of(c)).sum::<f64>() / c_sum;
            let (mut kept_p, mut kept_lo) = (0.0f64, 0.0f64);
            let mut any = false;
            for &(w3, c) in group.iter() {
                if c < k_tri {
                    continue;
                }
                let lo = p_bi(b, w3, kept_bi);
                let p = (f64::from(c) - tri_disc.of(c)).max(0.0) / c_sum + gamma * lo;
                kept_p += p;
                kept_lo += lo;
                any = true;
                trigrams.push(TrigramRecord { w1: a, w2: b, w3, log10_p: p.log10() as f32 });
            }
            if any {
                let alpha = renorm_alpha(kept_p, kept_lo, gamma);
                if let Ok(i) = kept_bi.binary_search_by_key(&pack2(a, b), |e| e.0) {
                    kept_bi[i].2 = alpha.log10() as f32;
                }
            }
            group.clear();
        };
        while let Some((k, c)) = r.next_rec()? {
            let (a, b, w3) = unpack3(k);
            if cur != Some((a, b)) {
                if let Some(prev) = cur {
                    flush(prev, &mut group, &mut kept_bi);
                }
                cur = Some((a, b));
            }
            group.push((w3, c));
        }
        if let Some(prev) = cur {
            flush(prev, &mut group, &mut kept_bi);
        }
    }
    let _ = fs::remove_file(&tri_path);
    eprintln!("[passC] kept {} trigrams (k_tri={k_tri}), {:?}", trigrams.len(), t0.elapsed());

    let unigrams: Vec<UnigramRecord> = (0..v)
        .map(|i| UnigramRecord {
            word_id: i as u32,
            log10_p: if i as u32 == bos { ARPA_IMPOSSIBLE } else { p_uni[i].log10() as f32 },
            backoff: bi_alpha[i].log10() as f32,
        })
        .collect();
    let bigrams: Vec<BigramRecord> = kept_bi
        .iter()
        .map(|&(k, lp, bo)| {
            let (w1, w2) = unpack2(k);
            BigramRecord { w1, w2, log10_p: lp, backoff: bo }
        })
        .collect();

    stats.put("kept_bigrams", bigrams.len());
    stats.put("kept_trigrams", trigrams.len());
    stats.put("bigram_cc_threshold", k_bi);
    stats.put("trigram_count_threshold", k_tri);
    stats.put("bigram_type_total", bigram_type_total);
    stats.put("discount_unigram", format!("{:?}", uni_disc.d));
    stats.put("discount_bigram", format!("{:?}", bi_disc.d));
    stats.put("discount_trigram", format!("{:?}", tri_disc.d));
    stats.put("smoothing", "interpolated modified Kneser-Ney (backoff-renormalized after count pruning)");

    let data = NgramData { flags: 0, vocab, unigrams, bigrams, trigrams };
    let bytes = encode_animengr_bytes(&data)
        .map_err(|e| io::Error::other(format!("encode ANIMENGR: {e}")))?;
    if let Some(parent) = args.out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.out, &bytes)?;
    stats.put("output_bytes", bytes.len());
    stats.put("elapsed_secs", format!("{:.1}", t0.elapsed().as_secs_f64()));
    eprintln!("[done] wrote {} ({} bytes) in {:?}", args.out.display(), bytes.len(), t0.elapsed());
    if let Some(sp) = &args.stats {
        fs::write(sp, stats.to_json())?;
    }
    Ok(())
}

/// 剪枝后的回退权重：未保留词的剩余质量 / 低阶分布在这些词上的剩余质量。
/// 全部保留时数学上恰好等于插值系数 γ；数值退化时回落到 γ。
fn renorm_alpha(kept_p: f64, kept_lo: f64, gamma: f64) -> f64 {
    let num = 1.0 - kept_p;
    let den = 1.0 - kept_lo;
    if num > 1e-9 && den > 1e-9 {
        (num / den).clamp(1e-9, 1.0e3)
    } else {
        gamma.max(1e-9)
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("anime-ngram-build: {e}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_splits_on_digits_and_punctuation() {
        let mut out = Vec::new();
        normalize_into("I'd like 2 TICKETS, please. Thanks\u{2019}!", &mut out);
        assert_eq!(out, vec![vec!["i'd", "like"], vec!["tickets"], vec!["please"], vec!["thanks"]]);
    }

    #[test]
    fn pack_roundtrip_and_ordering() {
        let k = pack3(5, 7, 9);
        assert_eq!(unpack3(k), (5, 7, 9));
        assert!(pack3(1, 2, 3) < pack3(1, 3, 0));
        assert_eq!(unpack2(pack2(MAX_VOCAB as u32, 3)), (MAX_VOCAB as u32, 3));
    }

    #[test]
    fn discounts_follow_modified_kn_formula() {
        let d = Discounts::estimate([1000, 400, 200, 120]);
        let y = 1000.0 / (1000.0 + 800.0);
        assert!((d.of(1) - (1.0 - 2.0 * y * 400.0 / 1000.0)).abs() < 1e-9);
        assert!(d.of(1) < d.of(2) && d.of(2) < d.of(3));
        assert_eq!(d.of(7), d.of(3));
    }

    #[test]
    fn threshold_respects_budget() {
        let mut h = vec![0u64; HIST_BUCKETS];
        h[1] = 100;
        h[2] = 50;
        h[3] = 20;
        h[10] = 5;
        assert_eq!(pick_threshold(&h, 1, 1000), 1);
        assert_eq!(pick_threshold(&h, 2, 1000), 2);
        assert_eq!(pick_threshold(&h, 1, 30), 3);
    }

    #[test]
    fn alpha_equals_gamma_without_pruning() {
        assert!((renorm_alpha(1.0, 1.0, 0.3) - 0.3).abs() < 1e-12);
        assert!((renorm_alpha(0.6, 0.2, 0.3) - 0.5).abs() < 1e-12);
    }
}
