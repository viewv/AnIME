//! 本机个人词汇与二元搭配学习存储（内存热缓存 + 异步批量 SQLite WAL 持久化）。
//!
//! 严格遵守 `.agents/contracts/data-format.md` §4 Schema：
//! - `user_words (word TEXT PRIMARY KEY, freq INTEGER NOT NULL DEFAULT 1, last_used INTEGER NOT NULL)`
//! - `user_bigrams (prev_word TEXT NOT NULL, word TEXT NOT NULL, freq INTEGER NOT NULL DEFAULT 1, last_used INTEGER NOT NULL, PRIMARY KEY (prev_word, word))`
//! 所有读取走纯内存 `RwLock`（零磁盘 I/O，不阻塞同步按键路径）；写入在 `privacy == false` 时
//! 先更新内存热表，再投递至后台 `sqlite3` WAL 持久化线程批量提交。

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{RwLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::format::NgramError;

const FLUSH_SENTINEL: &str = "__ANIME_FLUSH__";
const MAX_TOKEN_LEN: usize = 64;

#[derive(Debug, Clone)]
struct UserWordEntry {
    word: String,
    freq: u32,
    last_used: u64,
}

#[derive(Debug, Clone)]
struct UserBigramEntry {
    freq: u32,
    last_used: u64,
}

/// 原因注释：`user_words` 按**精确词形**独立计频（`so` / `So` / `SO` 互不吞并），否则一次 Shift 粘连的 `SO`
/// 会把 `so` 的全部历史频次收归名下并改写规范词形；L2 个性化加成只关心"这个词常不常用"，
/// 因此另维护大小写不敏感的汇总频次 `lower_freq` 供 `word_boost` / `bigram_boost` 查询。
#[derive(Debug, Default)]
struct UserMemory {
    words: HashMap<String, UserWordEntry>,
    lower_freq: HashMap<String, u32>,
    bigrams: HashMap<(String, String), UserBigramEntry>,
}

impl UserMemory {
    fn add_word(&mut self, word: &str, freq: u32, last_used: u64) {
        match self.words.get_mut(word) {
            Some(e) => {
                e.freq = e.freq.saturating_add(freq);
                e.last_used = e.last_used.max(last_used);
            }
            None => {
                self.words.insert(
                    word.to_string(),
                    UserWordEntry {
                        word: word.to_string(),
                        freq,
                        last_used,
                    },
                );
            }
        }
        let agg = self.lower_freq.entry(word.to_ascii_lowercase()).or_insert(0);
        *agg = agg.saturating_add(freq);
    }
}

#[derive(Debug)]
struct WordMutation {
    word: String,
    freq_delta: u32,
    timestamp: u64,
}

#[derive(Debug)]
struct BigramMutation {
    prev_word: String,
    word: String,
    freq_delta: u32,
    timestamp: u64,
}

enum DbCommand {
    UpsertBatch {
        words: Vec<WordMutation>,
        bigrams: Vec<BigramMutation>,
    },
    Flush(mpsc::Sender<()>),
    Shutdown,
}

struct WriterHandle {
    tx: mpsc::Sender<DbCommand>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for WriterHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(DbCommand::Shutdown);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// 线程安全的个人学习词频与搭配存储。
pub struct UserStore {
    memory: RwLock<UserMemory>,
    writer: Option<WriterHandle>,
}

impl std::fmt::Debug for UserStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (wc, bc) = {
            let g = self.memory.read().unwrap_or_else(|e| e.into_inner());
            (g.words.len(), g.bigrams.len())
        };
        f.debug_struct("UserStore")
            .field("word_count", &wc)
            .field("bigram_count", &bc)
            .field("sqlite_backed", &self.writer.is_some())
            .finish()
    }
}

impl Default for UserStore {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl UserStore {
    /// 纯内存个人学习模式（不启动 SQLite 子进程，适合单元测试或只读环境回退）。
    pub fn in_memory() -> Self {
        Self {
            memory: RwLock::new(UserMemory::default()),
            writer: None,
        }
    }

    /// 打开或创建 SQLite `user.db`（WAL 模式），预加载既有个人词频并启动后台批量写入线程。
    pub fn open_sqlite(path: &Path) -> Result<Self, NgramError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        init_sqlite_schema(path)?;
        let memory = load_initial_memory(path)?;
        let writer = spawn_sqlite_writer(path)?;

        Ok(Self {
            memory: RwLock::new(memory),
            writer: Some(writer),
        })
    }

    pub fn is_sqlite_backed(&self) -> bool {
        self.writer.is_some()
    }

    /// 显式刷新后台 SQLite 写入队列直至当前已入队事务全部落盘（纯内存模式为空操作）。
    pub fn flush(&self) {
        if let Some(writer) = &self.writer {
            let (ack_tx, ack_rx) = mpsc::channel();
            if writer.tx.send(DbCommand::Flush(ack_tx)).is_ok() {
                let _ = ack_rx.recv();
            }
        }
    }

    /// 查询单个词的个人高频权重加成（纯内存 O(1)，上限 `1.8`）。
    pub fn word_boost(&self, word: &str) -> f32 {
        let key = word.trim().to_ascii_lowercase();
        if key.is_empty() {
            return 0.0;
        }
        let guard = self.memory.read().unwrap_or_else(|e| e.into_inner());
        let Some(&freq) = guard.lower_freq.get(&key) else {
            return 0.0;
        };
        (0.35 * (1.0 + freq as f32).ln()).min(1.8)
    }

    /// 查询二元搭配 `(prev_word, word)` 的个人习惯权重加成（纯内存 O(1)，上限 `2.6`）。
    pub fn bigram_boost(&self, prev_word: Option<&str>, word: &str) -> f32 {
        let word_l = word.trim().to_ascii_lowercase();
        if word_l.is_empty() {
            return 0.0;
        }
        let guard = self.memory.read().unwrap_or_else(|e| e.into_inner());
        let uni = guard
            .lower_freq
            .get(&word_l)
            .map(|&f| (0.35 * (1.0 + f as f32).ln()).min(1.2))
            .unwrap_or(0.0);
        let bi = prev_word
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .and_then(|p| guard.bigrams.get(&(p, word_l)))
            .map(|e| (0.85 * (1.0 + e.freq as f32).ln()).min(2.6))
            .unwrap_or(0.0);
        (uni + bi).min(2.6)
    }

    pub fn word_freq(&self, word: &str) -> u32 {
        let key = word.trim().to_ascii_lowercase();
        if key.is_empty() {
            return 0;
        }
        self.memory
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .lower_freq
            .get(&key)
            .copied()
            .unwrap_or(0)
    }

    /// 精确词形（区分大小写）的累计频次，供 L1 动态层按词形独立晋升。
    pub fn exact_word_freq(&self, word: &str) -> u32 {
        self.memory
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .words
            .get(word.trim())
            .map(|e| e.freq)
            .unwrap_or(0)
    }

    pub fn bigram_freq(&self, prev: &str, word: &str) -> u32 {
        let p = prev.trim().to_ascii_lowercase();
        let w = word.trim().to_ascii_lowercase();
        if p.is_empty() || w.is_empty() {
            return 0;
        }
        self.memory
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .bigrams
            .get(&(p, w))
            .map(|e| e.freq)
            .unwrap_or(0)
    }

    /// 导出全部已学词条 `(精确词形, freq)`，供启动期注入 `LayeredLexicon::preload_dynamic_words`。
    pub fn snapshot_user_words(&self) -> Vec<(String, u32)> {
        let guard = self.memory.read().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<(String, u32)> = guard
            .words
            .values()
            .map(|e| (e.word.clone(), e.freq))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// 记录一次真实上屏学习：当 `privacy == true` 时绝不修改内存或磁盘 `user.db`。
    pub fn record_commit(&self, context_before: &str, committed_text: &str, privacy: bool) {
        if privacy {
            return;
        }
        let tokens = tokenize_words(committed_text);
        if tokens.is_empty() {
            return;
        }

        let ts = now_unix_secs();
        let mut prev_lower = context_prev_word_lower(context_before);
        let mut word_mutations = Vec::with_capacity(tokens.len());
        let mut bigram_mutations = Vec::with_capacity(tokens.len());

        {
            let mut guard = self.memory.write().unwrap_or_else(|e| e.into_inner());
            for tok in tokens {
                let lower = tok.to_ascii_lowercase();
                guard.add_word(&tok, 1, ts);
                word_mutations.push(WordMutation {
                    word: tok.clone(),
                    freq_delta: 1,
                    timestamp: ts,
                });

                if let Some(prev) = prev_lower.as_ref() {
                    let bkey = (prev.clone(), lower.clone());
                    match guard.bigrams.get_mut(&bkey) {
                        Some(entry) => {
                            entry.freq = entry.freq.saturating_add(1);
                            entry.last_used = entry.last_used.max(ts);
                        }
                        None => {
                            guard.bigrams.insert(
                                bkey,
                                UserBigramEntry {
                                    freq: 1,
                                    last_used: ts,
                                },
                            );
                        }
                    }
                    bigram_mutations.push(BigramMutation {
                        prev_word: prev.clone(),
                        word: lower.clone(),
                        freq_delta: 1,
                        timestamp: ts,
                    });
                }

                prev_lower = Some(lower);
            }
        }

        if let Some(writer) = &self.writer {
            let _ = writer.tx.send(DbCommand::UpsertBatch {
                words: word_mutations,
                bigrams: bigram_mutations,
            });
        }
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn context_prev_word_lower(context_before: &str) -> Option<String> {
    let trimmed_end = context_before.trim_end_matches([' ', '\t']);
    if trimmed_end.is_empty()
        || context_before.ends_with('\n')
        || context_before.ends_with('\r')
        || trimmed_end.ends_with('.')
        || trimmed_end.ends_with('!')
        || trimmed_end.ends_with('?')
    {
        return None;
    }
    tokenize_words(trimmed_end)
        .into_iter()
        .last()
        .map(|w| w.to_ascii_lowercase())
}

pub(crate) fn tokenize_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '\'' && c != '-') {
        let tok = raw.trim_matches(|c| c == '\'' || c == '-');
        if tok.is_empty() || tok.len() > MAX_TOKEN_LEN {
            continue;
        }
        if tok.chars().any(char::is_control) {
            continue;
        }
        if !tok.bytes().any(|b| b.is_ascii_alphabetic()) {
            continue;
        }
        out.push(tok.to_string());
    }
    out
}

fn sql_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch.is_control() || ch == '\0' {
            continue;
        }
        if ch == '\'' {
            out.push_str("''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn init_sqlite_schema(path: &Path) -> Result<(), NgramError> {
    let mut child = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| NgramError::Sqlite(format!("failed to launch sqlite3: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        let init_sql = "\
PRAGMA journal_mode=WAL;\n\
PRAGMA synchronous=NORMAL;\n\
PRAGMA busy_timeout=2000;\n\
CREATE TABLE IF NOT EXISTS user_words (\n\
    word        TEXT PRIMARY KEY,\n\
    freq        INTEGER NOT NULL DEFAULT 1,\n\
    last_used   INTEGER NOT NULL\n\
);\n\
CREATE TABLE IF NOT EXISTS user_bigrams (\n\
    prev_word   TEXT NOT NULL,\n\
    word        TEXT NOT NULL,\n\
    freq        INTEGER NOT NULL DEFAULT 1,\n\
    last_used   INTEGER NOT NULL,\n\
    PRIMARY KEY (prev_word, word)\n\
);\n";
        stdin
            .write_all(init_sql.as_bytes())
            .map_err(|e| NgramError::Sqlite(format!("failed writing schema SQL: {e}")))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| NgramError::Sqlite(format!("sqlite3 init wait failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NgramError::Sqlite(format!(
            "sqlite3 schema init failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(())
}

fn run_sqlite_select(path: &Path, sql: &str) -> Result<String, NgramError> {
    let output = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg("-separator")
        .arg("\t")
        .arg(path)
        .arg(sql)
        .output()
        .map_err(|e| NgramError::Sqlite(format!("sqlite3 select failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NgramError::Sqlite(format!(
            "sqlite3 select exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| NgramError::Sqlite("sqlite3 stdout contained invalid UTF-8".to_string()))
}

fn load_initial_memory(path: &Path) -> Result<UserMemory, NgramError> {
    let mut mem = UserMemory::default();

    let words_tsv = run_sqlite_select(path, "SELECT word, freq, last_used FROM user_words;")?;
    for line in words_tsv.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let (Some(word), Some(freq_s), Some(ts_s)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let word = word.trim();
        let Ok(freq) = freq_s.trim().parse::<u32>() else {
            continue;
        };
        let Ok(last_used) = ts_s.trim().parse::<u64>() else {
            continue;
        };
        if word.is_empty() {
            continue;
        }
        mem.add_word(word, freq, last_used);
    }

    let bigrams_tsv = run_sqlite_select(
        path,
        "SELECT prev_word, word, freq, last_used FROM user_bigrams;",
    )?;
    for line in bigrams_tsv.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let (Some(prev), Some(word), Some(freq_s), Some(ts_s)) =
            (cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        let prev = prev.trim().to_ascii_lowercase();
        let word = word.trim().to_ascii_lowercase();
        let Ok(freq) = freq_s.trim().parse::<u32>() else {
            continue;
        };
        let Ok(last_used) = ts_s.trim().parse::<u64>() else {
            continue;
        };
        if prev.is_empty() || word.is_empty() {
            continue;
        }
        let key = (prev, word);
        match mem.bigrams.get_mut(&key) {
            Some(existing) => {
                existing.freq = existing.freq.saturating_add(freq);
                existing.last_used = existing.last_used.max(last_used);
            }
            None => {
                mem.bigrams.insert(key, UserBigramEntry { freq, last_used });
            }
        }
    }

    Ok(mem)
}

fn spawn_sqlite_writer(path: &Path) -> Result<WriterHandle, NgramError> {
    let mut child = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| NgramError::Sqlite(format!("failed to start sqlite3 writer: {e}")))?;

    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| NgramError::Sqlite("missing sqlite3 stdin".to_string()))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| NgramError::Sqlite("missing sqlite3 stdout".to_string()))?;

    let (tx, rx) = mpsc::channel::<DbCommand>();
    let thread = thread::Builder::new()
        .name("anime-userdb-writer".to_string())
        .spawn(move || {
            writer_loop(&mut child, &mut child_stdin, child_stdout, rx);
        })
        .map_err(|e| NgramError::Sqlite(format!("failed spawning user_db writer thread: {e}")))?;

    Ok(WriterHandle {
        tx,
        thread: Some(thread),
    })
}

fn writer_loop(
    child: &mut Child,
    stdin: &mut std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    rx: mpsc::Receiver<DbCommand>,
) {
    let mut reader = BufReader::new(stdout);
    let _ = writeln!(
        stdin,
        "PRAGMA journal_mode=WAL;\nPRAGMA synchronous=NORMAL;\nPRAGMA busy_timeout=2000;\nSELECT '{FLUSH_SENTINEL}';"
    );
    let _ = stdin.flush();
    // 消费初始 journal_mode 输出与 sentinel
    let mut line_buf = String::new();
    while reader.read_line(&mut line_buf).is_ok() {
        let hit = line_buf.trim() == FLUSH_SENTINEL;
        line_buf.clear();
        if hit {
            break;
        }
    }

    let mut pending_flush: Option<mpsc::Sender<()>> = None;
    let mut should_shutdown = false;

    while let Ok(cmd) = rx.recv() {
        let mut word_batch = Vec::new();
        let mut bigram_batch = Vec::new();

        match cmd {
            DbCommand::UpsertBatch { words, bigrams } => {
                word_batch.extend(words);
                bigram_batch.extend(bigrams);
                // 顺便合并队列里紧邻的批次，减少事务提交次数
                for _ in 0..64 {
                    match rx.try_recv() {
                        Ok(DbCommand::UpsertBatch { words, bigrams }) => {
                            word_batch.extend(words);
                            bigram_batch.extend(bigrams);
                        }
                        Ok(DbCommand::Flush(ack)) => {
                            pending_flush = Some(ack);
                            break;
                        }
                        Ok(DbCommand::Shutdown) => {
                            should_shutdown = true;
                            break;
                        }
                        Err(_) => break,
                    }
                }
            }
            DbCommand::Flush(ack) => {
                pending_flush = Some(ack);
            }
            DbCommand::Shutdown => {
                should_shutdown = true;
            }
        }

        if !word_batch.is_empty() || !bigram_batch.is_empty() {
            let mut sql = String::from("BEGIN IMMEDIATE;\n");
            for wm in &word_batch {
                // 每个精确词形独立一行累加，绝不改名或删除其他大小写变体的历史行
                sql.push_str("INSERT INTO user_words (word, freq, last_used) VALUES (");
                sql.push_str(&sql_quote(&wm.word));
                sql.push_str(", ");
                sql.push_str(&wm.freq_delta.to_string());
                sql.push_str(", ");
                sql.push_str(&wm.timestamp.to_string());
                sql.push_str(") ON CONFLICT(word) DO UPDATE SET freq = freq + excluded.freq, last_used = MAX(last_used, excluded.last_used);\n");
            }
            for bm in &bigram_batch {
                sql.push_str(
                    "INSERT INTO user_bigrams (prev_word, word, freq, last_used) VALUES (",
                );
                sql.push_str(&sql_quote(&bm.prev_word));
                sql.push_str(", ");
                sql.push_str(&sql_quote(&bm.word));
                sql.push_str(", ");
                sql.push_str(&bm.freq_delta.to_string());
                sql.push_str(", ");
                sql.push_str(&bm.timestamp.to_string());
                sql.push_str(") ON CONFLICT(prev_word, word) DO UPDATE SET freq = freq + excluded.freq, last_used = MAX(last_used, excluded.last_used);\n");
            }
            sql.push_str("COMMIT;\n");
            let _ = stdin.write_all(sql.as_bytes());
        }

        if let Some(ack) = pending_flush.take() {
            let _ = writeln!(stdin, "SELECT '{FLUSH_SENTINEL}';");
            let _ = stdin.flush();
            loop {
                line_buf.clear();
                match reader.read_line(&mut line_buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if line_buf.trim() == FLUSH_SENTINEL {
                            break;
                        }
                    }
                }
            }
            let _ = ack.send(());
        } else if !word_batch.is_empty() || !bigram_batch.is_empty() {
            let _ = stdin.flush();
        }

        if should_shutdown {
            break;
        }
    }

    let _ = writeln!(stdin, ".quit");
    let _ = stdin.flush();
    let _ = child.wait();
}
